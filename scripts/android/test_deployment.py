"""Offline checks for shared endpoint generation and fail-closed profile validation."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import deployment
import direct_boot
import isolate
import lockdown
import phone


def fixture():
    return {'android':{'package':'org.example.termux','user':10,'root_command':'/system/bin/su'},
            'agent':{'name':'resident','gateway_port':43001,'mcp_port':43002,
                     'provider_host':'models.example.org','tools':['entity_list','routine_run']},
            'network':{'dns_servers':['192.0.2.53','198.51.100.53'],'lan_probe':'192.0.2.1:81','admin_port':9443}}


class DeploymentTests(unittest.TestCase):
    def test_one_profile_drives_firewall_gateway_policy_and_native_probe(self):
        p=deployment.Profile(fixture())
        paths=p.paths()
        with patch.object(isolate,'ROOT',paths['sandbox']),patch.object(phone,'DE_HOMEWIRE',paths['de']),patch.object(phone,'ZCONFIG',paths['zeroclaw']):
            launch=isolate.namespace_script(p)
            firewall=isolate.supervisor_script(p)
        for script in (launch,firewall):
            subprocess.run(['sh','-n'],input=script.encode(),check=True,capture_output=True)
        self.assertIn('--port 43001',launch)
        self.assertIn('resident /bin/probe resident',launch)
        self.assertIn('43002 9443 192.0.2.1 81 models.example.org',launch)
        self.assertIn('--dports 43002 -j ACCEPT',firewall)
        self.assertNotIn('42618',firewall)
        for address in p.network['dns_servers']:
            self.assertIn('-d '+address+' -p udp --dport 53',firewall)
            self.assertIn('-d '+address+' -p tcp --dport 53',firewall)
        self.assertLess(firewall.index('--dport 53'),firewall.index('for net in '))
        self.assertIn('ip6tables -A HW_AGENT -j REJECT',firewall)
        config={'agents':{'resident':{'enabled':True},'other':{'enabled':False}}}
        values={v['path']:v['value'] for v in lockdown.patches(config,'synthetic',p)}
        self.assertEqual(values['/mcp']['servers'][0]['url'],p.mcp_url)
        self.assertEqual(values['/risk_profiles']['homewire']['allowed_tools'],['time','homewire__entity_list','homewire__routine_run'])
        self.assertEqual(values['/agents']['resident']['risk_profile'],'homewire')
        self.assertEqual(values['/agents']['other']['mcp_bundles'],[])
        self.assertEqual(p.assistant_url,'http://127.0.0.1:43001/webhook?agent=resident')

    def test_invalid_profiles_cannot_generate_shell_rules_or_weaken_scopes(self):
        invalid=[('android','package','bad;id'),('android','root_command','/system/../data/su'),
                 ('android','user',True),('agent','name','../../config'),('agent','mcp_port',43001),
                 ('agent','provider_host','host;id'),('agent','tools',['entity_configure']),
                 ('agent','tools',['entity_list','entity_list']),('network','dns_servers',['127.0.0.1']),
                 ('network','dns_servers',['192.0.2.1;id']),('network','lan_probe','192.0.2.1:0'),
                 ('network','admin_port',43002)]
        for section,key,value in invalid:
            with self.subTest(key=key,value=value):
                c=fixture();c[section][key]=value
                with self.assertRaises(ValueError):deployment.Profile(c)

    def test_primary_and_secondary_layouts_and_private_permissions(self):
        c=fixture();p=deployment.Profile(c)
        self.assertTrue(p.paths()['ce'].startswith('/data/user/10/org.example.termux/'))
        self.assertTrue(p.paths()['de'].startswith('/data/user_de/10/org.example.termux/'))
        c['network']['dns_servers']=['192.168.20.53']
        c['android']['user']=0
        self.assertTrue(deployment.Profile(c).paths()['ce'].startswith('/data/data/org.example.termux/'))
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'deployment.json';path.write_text(json.dumps(c));path.chmod(0o644)
            with self.assertRaisesRegex(ValueError,'chmod 600'):deployment.load(path)
            path.chmod(0o600)
            with patch.object(deployment,'use',side_effect=lambda p:p):
                self.assertEqual(deployment.load(path).agent['name'],'resident')

    def test_api_profile_mismatch_fails_before_any_writes(self):
        p=deployment.Profile(fixture())
        class Fake:
            def sh(self,command):return json.dumps({'listen':'0.0.0.0:8443','assistant':{'url':p.assistant_url}}).encode()
        with self.assertRaises(ValueError):isolate.preflight(Fake(),p)

    def test_service_scripts_use_resolved_layout_and_owner_uid(self):
        with patch.object(phone,'CE_HOMEWIRE','/data/user/10/org.example.termux/files/home/homewire'),patch.object(phone,'PREFIX','/data/user/10/org.example.termux/files/usr'):
            script=phone.service_script(phone.CE_HOMEWIRE)
            self.assertNotIn('com.termux',script)
            self.assertIn('/data/user/10/org.example.termux/files/home/homewire',script)
        with patch.object(direct_boot,'ROOT','/data/user_de/10/org.example.termux/files/homewire'):
            script=direct_boot.wrapper(1010123,'/system/bin/su')
            self.assertNotIn('/debug_ramdisk',script)
            self.assertIn('1010123',script)
            subprocess.run(['sh','-n'],input=script.encode(),check=True,capture_output=True)

    def test_cli_policy_validation_rejects_expanded_grants(self):
        profile=deployment.Profile(fixture())
        target={p['path'][1:]:p['value'] for p in lockdown.patches({'agents':{'resident':{'enabled':True}}},'synthetic',profile)}
        class Fake:
            def __init__(self):self.files={}
            def put(self,path,data):self.files[path]=data
            def sh(self,command,data=None):
                if command.startswith('cat '):return self.files['/stage/config.toml']
                return b'ack'
        with patch.object(deployment,'_ACTIVE',profile):
            self.assertEqual(lockdown.validate_prepared(Fake(),'/stage',target,b'synthetic-key'),target)
            self.assertTrue(lockdown.preserves({'enabled':False,'new_default':0},{'enabled':False}))
            self.assertFalse(lockdown.preserves({'enabled':True},{'enabled':False}))
            target['risk_profiles']['homewire']['allowed_tools'].append('shell')
            with self.assertRaisesRegex(ValueError,'confinement'):
                lockdown.validate_prepared(Fake(),'/stage',target,b'synthetic-key')

    def test_policy_instructions_only_list_granted_capabilities(self):
        text=lockdown.instructions(('entity_list',))
        self.assertIn('`homewire__entity_list`',text)
        self.assertNotIn('`homewire__routine_run`',text)


if __name__=='__main__':unittest.main()
