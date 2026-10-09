#!/usr/bin/env python3
"""Operator-only ZeroClaw policy preparation for the native Android jail.

ZeroClaw 0.8.5 validates the prepared config through its own save path. Never print config
values, tokens or key material. Preserve encrypted provider keys, peers and channel enablement.
The scoped MCP bearer has the same private-file protection as Homewire's API token.
"""
import argparse
import copy
import json
import secrets
import tomllib
from pathlib import Path
import phone
import deployment

READ = ('entity_list', 'entity_status', 'entity_options', 'routine_list',
        'routine_plan', 'alert_list', 'history_series', 'history_query',
        'weather_status', 'shopping_list', 'radio_search', 'tts_media')
WRITE = ('light_set', 'switch_set', 'media_power', 'remote_key', 'routine_run',
         'media_play', 'media_command', 'shopping_update')
BLOCKED = ('shell', 'file_read', 'file_write', 'file_list', 'file_edit',
           'glob_search', 'content_search', 'model_routing_config', 'delegate',
           'spawn_subagent', 'cron_add', 'cron_update', 'cron_run', 'cron_remove',
           'execute_pipeline', 'http_request', 'web_fetch', 'web_search_tool',
           'browser', 'browser_open', 'backup', 'config', 'tool_search',
           'homewire__entity_configure')


def instructions(tools=None):
    tools = tools if tools is not None else READ + WRITE
    reads = tuple(n for n in READ if n in tools)
    writes = tuple(n for n in WRITE if n in tools)
    return ('# Active tool policy\n\n'
        'The operator has isolated this ZeroClaw deployment. Use the actual supplied tool schemas. '
        'Old conversation memories and identity notes about shell, files or other tool bundles are obsolete.\n\n'
        'Read tools: '+', '.join('`homewire__'+n+'`' for n in reads)+'.\n\n'
        'Trusted Homewire gateway controls: '+', '.join('`homewire__'+n+'`' for n in writes)+'.\n\n'
        'Telegram is read-only: control tools are excluded by the runtime. Telegram peers cannot administer agent settings. '
        'Shell, filesystem, arbitrary HTTP, browser, delegation, cron, camera, microphone, Google and photo tools are unavailable. '
        'Homewire device/security settings cannot be configured by this agent.\n\n'
        'Use the granted discovery tools and actual entity names. Never substitute shell commands or invent readings.\n')


def patches(config, token, profile=None):
    profile = profile or deployment.current()
    alias_name = profile.agent["name"]
    tools = profile.tools
    if not config.get('agents', {}).get(alias_name):
        raise ValueError('Expected the configured deployment agent')
    risk = {
        'level': 'supervised', 'workspace_only': True, 'allowed_commands': [],
        'allowed_roots': [], 'allowed_tools': ['time'] + ['homewire__' + n for n in tools],
        'auto_approve': ['time'] + ['homewire__' + n for n in tools],
        'excluded_tools': list(BLOCKED), 'always_ask': [],
        'block_high_risk_commands': True, 'require_approval_for_medium_risk': True,
        'shell_env_passthrough': [], 'sandbox_enabled': False,
        'forbidden_paths': ['/data', '/system', '/apex', '/proc', '/dev', '/bin',
                            '/state/zero/config.toml', '/state/zero/.secret_key'],
        'delegation_policy': {'mode': 'forbidden'},
    }
    agents = copy.deepcopy(config['agents'])
    for alias, agent in agents.items():
        agent['workspace'] = {'path': '/state/zero/agents/' + alias + '/workspace',
                              'unrestricted_filesystem': False, 'access': {}, 'read_memory_from': []}
        agent['mcp_bundles'] = ['homewire'] if alias == alias_name else []
        agent['delegates'] = []
        agent['delegate_same_risk_profile'] = False
        agent['skill_bundles'] = []
        agent['knowledge_bundles'] = []
        agent['cron_jobs'] = []
        agent['acp_enable_mcp'] = False
        agent['a2a'] = {'published': False, 'exposed_skills': []}
        # Auxiliary agents have no device, filesystem or system capabilities.
        agent['risk_profile'] = 'homewire' if alias == alias_name else 'default'
    baseline = copy.deepcopy(risk)
    baseline['allowed_tools'] = ['time']
    baseline['auto_approve'] = ['time']
    channels = copy.deepcopy(config.get('channels', {}))
    for channel in channels.get('telegram', {}).values():
        channel['excluded_tools'] = sorted(set(channel.get('excluded_tools', []) +
            list(BLOCKED) + ['homewire__' + n for n in WRITE]))
    peers = copy.deepcopy(config.get('peer_groups', {}))
    for group in peers.values():
        if str(group.get('channel', '')).split('.')[0] == 'telegram':
            if '*' in group.get('external_peers', []):
                raise ValueError('Telegram requires explicit peer IDs; wildcard access is refused')
            group['admin_for_agent_scope'] = False
    gateway = copy.deepcopy(config.get('gateway', {}))
    gateway['web_dist_dir'] = '/bin/web'
    values = {
        'agents': agents, 'risk_profiles': {'homewire': risk, 'default': baseline},
        'channels': channels, 'peer_groups': peers, 'gateway': gateway,
        'mcp': {'enabled': True, 'deferred_loading': False, 'servers': [
            {'name': 'homewire', 'transport': 'http', 'url': profile.mcp_url,
             'headers': {'Authorization': 'Bearer ' + token},
             'tool_timeout_secs': 300, 'max_response_bytes': 65536}]},
        'mcp_bundles': {'homewire': {'servers': ['homewire']}},
        'scheduler': dict(config.get('scheduler', {}), enabled=False),
        'runtime': dict(config.get('runtime', {}), shell='/nonexistent-shell'),
    }
    return [{'op': 'add', 'path': '/' + k, 'value': v} for k, v in values.items()]


def preserves(actual, expected):
    """CLI normalization may add defaults; every explicitly prepared value stays fixed."""
    if isinstance(expected,dict):
        return isinstance(actual,dict) and all(k in actual and preserves(actual[k],v) for k,v in expected.items())
    if isinstance(expected,list):
        return isinstance(actual,list) and len(actual)==len(expected) and all(preserves(a,e) for a,e in zip(actual,expected))
    return type(actual) is type(expected) and actual==expected


def validate_prepared(p, destination, target, key):
    """Use the pinned CLI's save path before changing the active policy."""
    prepared=to_toml(target)
    if tomllib.loads(prepared)!=target:raise ValueError('TOML round-trip changed configuration')
    p.sh('umask 077; mkdir ' + phone.q(destination))
    p.put(destination + '/config.toml', prepared.encode())
    p.put(destination + '/.secret_key', key)
    result = p.sh('zeroclaw --config-dir ' + phone.q(destination) + ' config patch -',
                  json.dumps([{'op':'replace','path':'/scheduler/enabled', 'value':False}]).encode())
    if not result: raise ValueError('Config patch produced no acknowledgement')
    updated=tomllib.loads(p.sh('cat ' + phone.q(destination + '/config.toml')).decode())
    profile=deployment.current()
    agent=updated.get('agents',{}).get(profile.agent['name'],{})
    risk=updated.get('risk_profiles',{}).get('homewire',{})
    if agent.get('workspace',{}).get('unrestricted_filesystem') is not False or agent.get('risk_profile')!='homewire':
        raise ValueError('Workspace confinement did not persist')
    if not preserves(updated.get('risk_profiles'),target['risk_profiles']) or not preserves(updated.get('agents'),target['agents']):
        raise ValueError('Prepared security policy did not persist')
    if risk.get('allowed_commands')!=[] or risk.get('level')!='supervised' or set(risk.get('allowed_tools',[]))!= {'time'}|{'homewire__'+n for n in profile.tools}:
        raise ValueError('Agent tool confinement did not persist')
    if updated.get('scheduler',{}).get('enabled') is not False or not preserves(updated.get('mcp',{}).get('servers'),target['mcp']['servers']):
        raise ValueError('Scheduler or MCP confinement did not persist')
    if not preserves(updated.get('channels'),target['channels']) or not preserves(updated.get('peer_groups'),target['peer_groups']):
        raise ValueError('Channel or peer confinement did not persist')
    return updated


def prepare(p, destination):
    original=p.sh('cat '+phone.q(phone.ZCONFIG))
    config=tomllib.loads(original.decode())
    token=secrets.token_hex(32)
    target=copy.deepcopy(config)
    for patch in patches(config,token,deployment.current()):target[patch['path'][1:]]=patch['value']
    key=p.sh('cat '+phone.q(str(Path(phone.ZCONFIG).parent / '.secret_key')))
    validate_prepared(p,destination,target,key)
    print('Validated restricted config prepared; credentials are not displayed.')
    return token


def to_toml(config):
    """Small strict writer for the config's TOML data types; no extra dependency."""
    def value(v):
        if isinstance(v,str):return json.dumps(v,ensure_ascii=False)
        if isinstance(v,bool):return 'true' if v else 'false'
        if isinstance(v,(int,float)):return repr(v)
        if isinstance(v,list):return '['+', '.join(value(x) for x in v)+']'
        if isinstance(v,dict):return '{'+', '.join(json.dumps(k)+' = '+value(x) for k,x in v.items())+'}'
        raise ValueError('Unsupported TOML value type')
    lines=[]
    def table(path,data):
        if path:lines.append('['+'.'.join(json.dumps(k) for k in path)+']')
        for k,v in data.items():
            if not isinstance(v,dict):lines.append(json.dumps(k)+' = '+value(v))
        lines.append('')
        for k,v in data.items():
            if isinstance(v,dict):table(path+[k],v)
    table([],config)
    return '\n'.join(lines)+'\n'


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--serial', required=True)
    deployment.add_argument(ap,required=True)
    ap.add_argument('--destination', required=True)
    args = ap.parse_args()
    deployment.load(args.deployment)
    prepare(phone.Phone(args.serial), args.destination)
