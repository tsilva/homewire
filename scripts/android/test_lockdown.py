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

    def test_operator_grant_removes_old_write_denials_and_preserves_channel_access(self):
        profile_value=copy.deepcopy(self.profile.value)
        profile_value['agent']['telegram_control_tools']=['switch_set']
        profile=deployment.Profile(profile_value)
        config={'agents':{'phone':{'enabled':True}},'channels':{'telegram':{
            'phone':{'enabled':True,'bot_token':'synthetic','excluded_tools':
                ['homewire__'+n for n in lockdown.WRITE]+['custom_denial']},
            'disabled':{'enabled':False}}},'peer_groups':{'telegram_phone':{
                'channel':'telegram.phone','external_peers':['123'],'admin_for_agent_scope':True}}}
        values={p['path']:p['value'] for p in lockdown.patches(config,'synthetic',profile)}
        channels=values['/channels']['telegram']
        self.assertTrue(channels['phone']['enabled'])
        self.assertFalse(channels['disabled']['enabled'])
        self.assertEqual(channels['phone']['bot_token'],'synthetic')
        excluded=channels['phone']['excluded_tools']
        self.assertNotIn('homewire__switch_set',excluded)
        self.assertIn('custom_denial',excluded)
        self.assertTrue(set(lockdown.BLOCKED)<=set(excluded))
        self.assertTrue({'homewire__'+n for n in lockdown.WRITE if n!='switch_set'}<=set(excluded))
        self.assertEqual(values['/peer_groups']['telegram_phone']['external_peers'],['123'])
        self.assertFalse(values['/peer_groups']['telegram_phone']['admin_for_agent_scope'])
        text=lockdown.instructions(profile.tools,profile.telegram_controls)
        self.assertIn('Telegram may use these operator-granted controls: `homewire__switch_set`.',text)
        self.assertNotIn('Telegram is read-only',text)
        # Reapplying is stable; revoking the explicit grant restores read-only access.
        updated={p['path'][1:]:p['value'] for p in lockdown.patches(config,'synthetic',profile)}
        self.assertEqual(lockdown.patches(updated,'synthetic',profile),lockdown.patches(config,'synthetic',profile))
        revoked={p['path']:p['value'] for p in lockdown.patches(updated,'synthetic',self.profile)}
        self.assertIn('homewire__switch_set',revoked['/channels']['telegram']['phone']['excluded_tools'])

    def test_telegram_peers_cannot_administer_and_wildcards_fail_closed(self):
        config={'agents':{'phone':{'enabled':True}},'peer_groups':{
            'telegram_phone':{'channel':'telegram.phone','external_peers':['123'],
                              'admin_for_agent_scope':True}}}
        values={p['path']:p['value'] for p in lockdown.patches(config,'synthetic',self.profile)}
        self.assertFalse(values['/peer_groups']['telegram_phone']['admin_for_agent_scope'])
        config['peer_groups']['telegram_phone']['external_peers']=['*']
        with self.assertRaises(ValueError):lockdown.patches(config,'synthetic',self.profile)

    def test_tv_grants_keep_general_settings_and_other_writes_blocked(self):
        controls = ['switch_set', 'media_power', 'remote_key', 'media_launch', 'media_text']
        values = copy.deepcopy(self.profile.value)
        values['agent']['telegram_control_tools'] = controls
        profile = deployment.Profile(values)
        config = {'agents': {'phone': {'enabled': True}}, 'channels': {'telegram': {
            'phone': {'enabled': True, 'excluded_tools': ['custom_denial']}}}}
        patches = {p['path']: p['value'] for p in lockdown.patches(config, 'synthetic', profile)}
        excluded = patches['/channels']['telegram']['phone']['excluded_tools']
        self.assertFalse(set(excluded) & {'homewire__' + n for n in controls})
        for name in ['entity_configure', 'routine_run', 'light_set', 'media_play', 'media_command', 'remote_hold', 'media_voice', 'media_activity']:
            self.assertIn('homewire__' + name, excluded)
        self.assertIn('custom_denial', excluded)
        self.assertIn('homewire__entity_configure', patches['/risk_profiles']['homewire']['excluded_tools'])

    def test_isolated_deployment_refuses_legacy_grant_before_reading_config(self):
        class IsolatedPhone:
            def sh(self,command):
                if 'zeroclaw-isolated' not in command:
                    raise AssertionError('Legacy configuration was accessed')
                return b'yes\n'
        with self.assertRaises(ValueError):phone.connect(IsolatedPhone(),'phone')

    def test_network_wake_requires_explicit_channel_grant_and_can_be_revoked(self):
        values = copy.deepcopy(self.profile.value)
        values['agent']['tools'] = ['wake_list', 'wake_on_lan', 'entity_status']
        config = {'agents': {'phone': {'enabled': True}}, 'channels': {'telegram': {
            'phone': {'enabled': False, 'excluded_tools': ['custom_denial']}}}}
        denied = {p['path'][1:]: p['value'] for p in lockdown.patches(config, 'synthetic', deployment.Profile(values))}
        self.assertIn('homewire__wake_on_lan', denied['channels']['telegram']['phone']['excluded_tools'])
        values['agent']['telegram_control_tools'] = ['wake_on_lan']
        profile = deployment.Profile(values)
        granted = {p['path'][1:]: p['value'] for p in lockdown.patches(denied, 'synthetic', profile)}
        channel = granted['channels']['telegram']['phone']
        self.assertFalse(channel['enabled'])
        self.assertNotIn('homewire__wake_on_lan', channel['excluded_tools'])
        self.assertIn('custom_denial', channel['excluded_tools'])
        self.assertIn('homewire__media_power', channel['excluded_tools'])
        self.assertEqual(set(granted['risk_profiles']['homewire']['allowed_tools']),
                         {'time', 'homewire__wake_list', 'homewire__wake_on_lan', 'homewire__entity_status'})
        values['agent']['telegram_control_tools'] = []
        revoked = {p['path'][1:]: p['value'] for p in lockdown.patches(granted, 'synthetic', deployment.Profile(values))}
        self.assertIn('homewire__wake_on_lan', revoked['channels']['telegram']['phone']['excluded_tools'])
        instructions = lockdown.instructions(profile.tools, profile.telegram_controls)
        self.assertIn('Never automatically resend the packet', instructions)
        self.assertIn('verify its pinned identity and power state', instructions)
        self.assertIn('pending or unknown, not failed', instructions)


if __name__=='__main__':unittest.main()
