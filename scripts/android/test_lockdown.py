import copy
import unittest
import tomllib
import struct
import lockdown
import phone
import isolate
import deployment


class LockdownTests(unittest.TestCase):
    def setUp(self):
        self.profile = deployment.Profile({'android': {'package': 'com.termux', 'user': 0}, 'agent': {'name': 'phone', 'gateway_port': 42617, 'mcp_port': 42620, 'transcription_port': None, 'provider_host': 'provider.example'}, 'network': {'dns_servers': ['192.0.2.1'], 'lan_probe': '192.0.2.1:80', 'admin_port': 8443}})
    def test_static_binary_cannot_silently_bypass_the_preload_guard(self):
        header=bytearray(64)
        header[:6]=b'\x7fELF\x02\x01'
        struct.pack_into('<H',header,18,183)
        struct.pack_into('<Q',header,32,64)
        struct.pack_into('<HH',header,54,56,1)
        class BinaryPhone:
            def sh(self,command):
                return bytes(header) if command.startswith('head ') else bytes(56)
        with self.assertRaisesRegex(ValueError,'Static binaries'):
            isolate.require_supported_binary(BinaryPhone())

    def test_writer_round_trips_arrays_nested_maps_and_escaped_strings(self):
        values={'agents':{'a.b':{'enabled':True,'extra':'quote" and\nnewline'}},
                'mcp':{'servers':[{'name':'homewire','headers':{'Authorization':'synthetic'}}]},
                'rate':0.5,'budget':20,'unused':[]}
        self.assertEqual(tomllib.loads(lockdown.to_toml(values)),values)
    def test_grants_are_explicit_and_telegram_cannot_write(self):
        config={'agents':{'phone':{},'other':{'enabled':False}},
                'channels':{'telegram':{'phone':{'enabled':False,'bot_token':'synthetic'}}}}
        config['agents']['phone']['enabled']=True
        before=copy.deepcopy(config)
        values={p['path']:p['value'] for p in lockdown.patches(config,'synthetic-token',self.profile)}
        self.assertEqual(config,before)
        self.assertEqual(values['/risk_profiles']['homewire']['allowed_commands'],[])
        self.assertFalse(values['/agents']['phone']['workspace']['unrestricted_filesystem'])
        self.assertEqual(values['/agents']['phone']['mcp_bundles'],['homewire'])
        self.assertEqual(values['/agents']['other']['mcp_bundles'],[])
        self.assertEqual(len(values['/mcp']['servers']),1)
        tg=values['/channels']['telegram']['phone']
        self.assertFalse(tg['enabled'])
        self.assertEqual(tg['bot_token'],'synthetic')
        for name in lockdown.WRITE:
            self.assertIn('homewire__'+name,tg['excluded_tools'])
        self.assertIn('homewire__entity_configure',values['/risk_profiles']['homewire']['excluded_tools'])
        self.assertFalse(values['/scheduler']['enabled'])

    def test_missing_phone_never_synthesizes_a_privileged_agent(self):
        with self.assertRaises(ValueError):lockdown.patches({'agents':{}},'synthetic',self.profile)

    def test_telegram_peers_cannot_administer_and_wildcards_fail_closed(self):
        config={'agents':{'phone':{'enabled':True}},'peer_groups':{
            'telegram_phone':{'channel':'telegram.phone','external_peers':['123'],
                              'admin_for_agent_scope':True}}}
        values={p['path']:p['value'] for p in lockdown.patches(config,'synthetic',self.profile)}
        self.assertFalse(values['/peer_groups']['telegram_phone']['admin_for_agent_scope'])
        config['peer_groups']['telegram_phone']['external_peers']=['*']
        with self.assertRaises(ValueError):lockdown.patches(config,'synthetic',self.profile)

    def test_isolated_deployment_refuses_legacy_grant_before_reading_config(self):
        class IsolatedPhone:
            def sh(self,command):
                if 'zeroclaw-isolated' not in command:
                    raise AssertionError('Legacy configuration was accessed')
                return b'yes\n'
        with self.assertRaises(ValueError):phone.connect(IsolatedPhone(),'phone')


if __name__=='__main__':unittest.main()
