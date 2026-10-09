#!/usr/bin/env python3
"""Grant the Homewire HTTPS frontend access to one local ZeroClaw agent.
Stores a dedicated gateway token only in the private server config, never in the
companion APK. Existing model credentials and paired clients remain unchanged.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import time
import tomllib
import phone
import deployment


def configure(p, path, agent):
    profile = deployment.current()
    if agent != profile.agent["name"]: raise ValueError("Agent must match the deployment profile")
    phone.require_legacy_zeroclaw(p)
    cfg = json.loads(path.read_text())
    original = p.sh('cat ' + phone.q(phone.ZCONFIG))
    z = tomllib.loads(original.decode())
    if agent not in z.get('agents', {}):
        raise ValueError('Unknown ZeroClaw agent')
    token = cfg.get('assistant', {}).get('bearer_token') or 'zc_' + secrets.token_hex(32)
    hashed = hashlib.sha256(token.encode()).hexdigest()
    tokens = list(z.get('gateway', {}).get('paired_tokens', []))
    if hashed not in tokens:
        tokens.append(hashed)
        match = re.search(r'(?ms)^\[gateway\]\n(.*?)(?=^\[|\Z)', original.decode())
        if not match:
            raise ValueError('Expected the existing simple gateway section')
        body = match[1]
        line = 'paired_tokens = ' + json.dumps(tokens)
        if re.search(r'^paired_tokens\s*=.*$', body, re.M):
            body = re.sub(r'^paired_tokens\s*=.*$', line, body, flags=re.M)
        else:
            body += '\n' + line + '\n'
        text = original.decode()[:match.start(1)] + body + original.decode()[match.end(1):]
        expected = tomllib.loads(original.decode())
        expected.setdefault('gateway', {})['paired_tokens'] = tokens
        if tomllib.loads(text) != expected:
            raise ValueError('Unexpected ZeroClaw changes')
        if p.sh('cat ' + phone.q(phone.ZCONFIG)) != original:
            raise ValueError('ZeroClaw configuration changed during preparation')
        p.sh('umask 077; cp -p ' + phone.q(phone.ZCONFIG) + ' ' + phone.q(phone.ZCONFIG + '.before-homewire-assistant-' + str(int(time.time()))))
        p.put(phone.ZCONFIG + '.homewire-new', text.encode())
        p.sh('mv -f ' + phone.q(phone.ZCONFIG + '.homewire-new') + ' ' + phone.q(phone.ZCONFIG))
        p.sh('sv restart ' + phone.q(phone.PREFIX + '/var/service/zeroclaw'))
    cfg['assistant'] = {'url': profile.assistant_url, 'bearer_token': token}
    # Each companion still needs an explicit assistant:true permission in clients.
    os.chmod(path, 0o600)
    path.write_text(json.dumps(cfg, indent=2) + '\n')
    print('Dedicated local assistant gateway token configured; model credentials preserved.')


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--serial', required=True)
    deployment.add_argument(ap,required=True)
    ap.add_argument('--api-config', type=Path, required=True)
    ap.add_argument('--agent', required=True)
    args = ap.parse_args()
    deployment.load(args.deployment)
    configure(phone.Phone(args.serial), args.api_config, args.agent)
