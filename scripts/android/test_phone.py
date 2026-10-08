"""Offline checks for config preservation and deployment input rejection."""
import contextlib
import io
import tempfile
import tomllib
import unittest
from pathlib import Path
import phone
import direct_boot
import pair_digi
from unittest.mock import patch


class FakePhone:
    def __init__(self, config):
        self.original = config.encode()
        self.writes = {}
        self.commands = []

    def sh(self, command, data=None):
        self.commands.append(command)
        return self.original if command.startswith('cat ') else b''

    def put(self, path, data, mode='600'):
        self.writes[path] = data


class DeploymentChecks(unittest.TestCase):
    def test_update_keeps_schedules_and_monitoring_and_requires_explicit_revocation(self):
        flags = b'--allow-control\n--schedule\n--watch-lights\n'
        self.assertEqual(phone.service_flags(flags), flags)
        self.assertEqual(phone.service_flags(flags, True), flags)
        with self.assertRaises(ValueError):
            phone.service_flags(flags, False)
        self.assertEqual(phone.service_flags(b'--watch-lights\n', False), b'--watch-lights\n')
    def test_existing_credentials_and_other_agents_survive_mcp_grant(self):
        text = '''[providers.models.phone]
api_key = "synthetic-private-value"
[agents.phone]
mcp_bundles = ["existing"]
[agents.other]
mcp_bundles = []
[mcp_bundles.existing]
servers = ["existing"]
[[mcp.servers]]
name = "existing"
command = "/unchanged"
'''
        p = FakePhone(text)
        with contextlib.redirect_stdout(io.StringIO()):
            phone.connect(p, 'phone')
        updated = tomllib.loads(p.writes[phone.ZCONFIG + '.homewire-new'].decode())
        self.assertEqual(updated['providers'], tomllib.loads(text)['providers'])
        self.assertEqual(updated['agents']['other']['mcp_bundles'], [])
        self.assertEqual(updated['agents']['phone']['mcp_bundles'], ['existing', 'homewire'])
        again = FakePhone(p.writes[phone.ZCONFIG + '.homewire-new'].decode())
        with contextlib.redirect_stdout(io.StringIO()):
            phone.connect(again, 'phone')
        self.assertEqual(again.writes, {})
        self.assertFalse(any('sv restart' in c for c in again.commands))

    def test_direct_boot_moves_only_the_known_managed_mcp_server(self):
        legacy = {'name':'homewire','transport':'stdio','command':phone.CE_HOMEWIRE+'/current/homewire','args':['bridge',phone.CE_HOMEWIRE+'/run/homewire.sock'],'tool_timeout_secs':300}
        import json
        text='[agents.phone]\nmcp_bundles=["homewire"]\n[mcp_bundles.homewire]\nservers=["homewire"]\n[[mcp.servers]]\n'+''.join(k+' = '+json.dumps(v)+'\n' for k,v in legacy.items())
        p=FakePhone(text)
        with patch.object(phone,'HOMEWIRE',phone.DE_HOMEWIRE),contextlib.redirect_stdout(io.StringIO()):phone.connect(p,'phone')
        new=tomllib.loads(p.writes[phone.ZCONFIG+'.homewire-new'].decode())
        self.assertEqual(new['mcp']['servers'][0]['command'],phone.DE_HOMEWIRE+'/current/homewire')
        self.assertNotIn(phone.CE_HOMEWIRE,direct_boot.wrapper(10186))
        self.assertIn('10186',direct_boot.wrapper(10186))

    def test_pairing_codec_rejects_truncation_and_duplicate_fields(self):
        packet=pair_digi.integer(1,2)+pair_digi.integer(2,200)+pair_digi.nested(40,b'abc')
        self.assertEqual(pair_digi.decode(packet),{1:2,2:200,40:b'abc'})
        with self.assertRaises(ValueError):pair_digi.decode(packet[:-1])
        with self.assertRaises(ValueError):pair_digi.decode(pair_digi.integer(1,2)*2)

    def test_shutdown_validates_both_pids_before_signaling_either(self):
        with patch.object(direct_boot,'root_sh',side_effect=[b'123\n',RuntimeError('Foreign process')]) as root:
            with self.assertRaises(RuntimeError):direct_boot.stop('synthetic-serial')
            self.assertEqual(root.call_count,2)
            self.assertFalse(any('kill ' in c.args[1] for c in root.call_args_list))

    def test_conflicting_server_fails_without_writing(self):
        p = FakePhone('[agents.phone]\nmcp_bundles=[]\n[[mcp.servers]]\nname="homewire"\ncommand="/other"\n')
        with self.assertRaises(ValueError):
            phone.connect(p, 'phone')
        self.assertEqual(p.writes, {})

    def test_nested_test_overrides_are_rejected_and_unrelated_files_ignored(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / 'plugs.json').write_text('{"plugs":[{"connect":"127.0.0.1:1234"}]}')
            with self.assertRaises(ValueError):
                phone.validate_configs(root)
            (root / 'plugs.json').write_text('{"plugs":[{"host":"192.168.1.10"}]}')
            (root / 'plugs-test.json').write_text('not copied')
            (root / 'plugs.state.json').write_text('not copied')
            self.assertEqual([name for name, _ in phone.validate_configs(root)], ['plugs.json'])


if __name__ == '__main__':
    unittest.main()
