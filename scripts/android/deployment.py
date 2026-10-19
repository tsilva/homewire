"""Validated operator-only installation profile; no resident config service."""
import ipaddress
import json
from pathlib import Path
import re
import configuration
import subprocess

DEFAULT_ANDROID = {'package': 'com.termux', 'user': 0}
_ACTIVE = None
_ROOT_CACHE = {}


def label(value):
    return isinstance(value, str) and re.fullmatch(r'[A-Za-z0-9_-]{1,32}', value) is not None


def port(value):
    return type(value) is int and 1 <= value <= 65535


def absolute(value):
    return isinstance(value, str) and re.fullmatch(r'/[A-Za-z0-9_./-]+', value) is not None and not any(part in ('.','..') for part in value.split('/'))


def layout(settings=None):
    a = settings or DEFAULT_ANDROID
    ce = ('/data/data/' if a['user']==0 else '/data/user/'+str(a['user'])+'/') + a['package'] + '/files'
    de = '/data/user_de/' + str(a['user']) + '/' + a['package'] + '/files'
    return {'ce':ce+'/home/homewire', 'de':de+'/homewire', 'prefix':ce+'/usr',
            'zeroclaw':a.get('zeroclaw_config',ce+'/home/.zeroclaw/config.toml'),
            'sandbox':ce+'/home/zeroclaw-sandbox'}


class Profile:
    def __init__(self, value):
        if not isinstance(value, dict) or set(value) != {'android', 'agent', 'network'}:
            raise ValueError('Deployment requires android, agent and network sections')
        a, z, n = value['android'], value['agent'], value['network']
        if not all(isinstance(v, dict) for v in (a, z, n)):
            raise ValueError('Invalid deployment sections')
        if set(a) - {'package', 'user', 'root_command', 'zeroclaw_config'} or not {'package','user'} <= set(a):
            raise ValueError('Invalid Android layout fields')
        if not isinstance(a['package'], str) or not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)+', a['package']):
            raise ValueError('Invalid Android package')
        if type(a['user']) is not int or not 0 <= a['user'] <= 999:
            raise ValueError('Invalid Android user')
        if any(not absolute(a[k]) for k in ('root_command','zeroclaw_config') if k in a):
            raise ValueError('Android overrides require normalized absolute paths')
        required = {'name','gateway_port','mcp_port','provider_host'}
        if not required <= set(z) or set(z) - required - {'transcription_port','tools','telegram_control_tools'}:
            raise ValueError('Invalid agent deployment fields')
        if not label(z['name']) or not isinstance(z['provider_host'], str) or not re.fullmatch(r'[A-Za-z0-9](?:[A-Za-z0-9.-]{0,251}[A-Za-z0-9])?', z['provider_host']):
            raise ValueError('Invalid agent alias or provider hostname')
        ports = [z['gateway_port'], z['mcp_port']]
        if z.get('transcription_port') is not None:
            ports.append(z['transcription_port'])
        if not all(port(p) for p in ports) or len(set(ports)) != len(ports):
            raise ValueError('Agent ports must be valid and distinct')
        if set(n) != {'dns_servers','lan_probe','admin_port'} or not port(n['admin_port']) or n['admin_port'] in ports:
            raise ValueError('Invalid deployment network fields')
        dns = n['dns_servers']
        if not isinstance(dns, list) or not 1 <= len(dns) <= 4 or not all(isinstance(v,str) for v in dns) or len(set(dns)) != len(dns):
            raise ValueError('Configure 1–4 unique IPv4 DNS servers')
        def ipv4(v):
            try: address = ipaddress.IPv4Address(v)
            except (ValueError, TypeError): raise ValueError('Invalid deployment IPv4 address') from None
            if address.is_unspecified or address.is_multicast or address.is_loopback or address.is_reserved or address.is_link_local:
                raise ValueError('Unsupported DNS/probe address')
            return str(address)
        for address in dns: ipv4(address)
        if not isinstance(n['lan_probe'],str): raise ValueError('Invalid LAN probe endpoint')
        parts = n['lan_probe'].split(':')
        if len(parts)!=2 or not parts[1].isascii() or not parts[1].isdecimal() or not port(int(parts[1])):
            raise ValueError('LAN probe must be IPv4:PORT')
        ipv4(parts[0])
        import lockdown
        tools = z.get('tools', list(lockdown.READ + lockdown.WRITE))
        if not isinstance(tools,list) or not tools or not all(isinstance(t,str) for t in tools) or len(set(tools))!=len(tools) or not set(tools)<=set(lockdown.READ+lockdown.WRITE):
            raise ValueError('Agent tools must be a subset of supported Homewire capabilities')
        telegram_controls = z.get('telegram_control_tools', [])
        if not isinstance(telegram_controls,list) or not all(isinstance(t,str) for t in telegram_controls) or len(set(telegram_controls))!=len(telegram_controls) or not set(telegram_controls)<=set(tools)&set(lockdown.WRITE):
            raise ValueError('Telegram controls must be a unique subset of granted Homewire write tools')
        self.value = json.loads(json.dumps(value))
        self.android = self.value['android']
        self.agent = self.value['agent']
        self.network = self.value['network']
        self.tools = tuple(tools)
        self.telegram_controls = tuple(telegram_controls)

    @property
    def endpoint(self): return '127.0.0.1:' + str(self.agent['mcp_port'])

    @property
    def mcp_url(self): return 'http://' + self.endpoint + '/mcp'

    @property
    def assistant_url(self):
        return 'http://127.0.0.1:' + str(self.agent['gateway_port']) + '/webhook?agent=' + self.agent['name']

    def paths(self): return layout(self.android)


def current():
    if _ACTIVE is None: raise ValueError('Supply --config with a private home.json deployment section')
    return _ACTIVE


def android(): return _ACTIVE.android if _ACTIVE else DEFAULT_ANDROID


def use(profile):
    global _ACTIVE
    _ACTIVE = profile
    import phone
    paths = profile.paths()
    phone.CE_HOMEWIRE, phone.DE_HOMEWIRE = paths['ce'], paths['de']
    phone.HOMEWIRE, phone.PREFIX, phone.ZCONFIG = paths['ce'], paths['prefix'], paths['zeroclaw']
    phone.SERVICE = phone.PREFIX + '/var/service/homewire'
    import direct_boot
    direct_boot.ROOT = paths['de']
    return profile


def load(path):
    path=Path(path)
    value=configuration.load(path)
    unified='version' in value
    if unified:
        configuration.validate(value)
        value=value.get('deployment')
    profile=Profile(value)
    profile.config_file=path if unified else None
    if profile.agent["provider_host"].endswith(".example") or any(ipaddress.IPv4Address(a) in ipaddress.IPv4Network("192.0.2.0/24") for a in profile.network["dns_servers"]):
        raise ValueError("Replace documentation-only deployment placeholders")
    return use(profile)


def add_argument(parser, required=False):
    parser.add_argument('--config', '--deployment', dest='deployment', type=Path, required=required, help='Private home.json (legacy deployment.json accepted; chmod 600)')


def root_executable(serial):
    configured=android().get('root_command')
    if configured: return configured
    if serial not in _ROOT_CACHE:
        result=subprocess.run(['adb','-s',serial,'shell','-T','command -v su'],capture_output=True,check=True,timeout=10)
        value=result.stdout.decode().strip()
        if not absolute(value): raise ValueError('Cannot discover root executable; configure android.root_command')
        _ROOT_CACHE[serial]=value
    return _ROOT_CACHE[serial]
