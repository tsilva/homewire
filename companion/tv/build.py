"""Build with an Infisical-injected persistent signing identity, never log it."""
import base64
import os
from pathlib import Path
import subprocess


def main():
    key = base64.b64decode(os.environ['HOMEWIRE_TV_SIGNING_KEY_B64'], validate=True)
    password = os.environ['HOMEWIRE_TV_SIGNING_PASSWORD'].encode()
    if not 1000 <= len(key) <= 16384 or not 16 <= len(password) <= 256:
        raise ValueError('Invalid TV signing identity')
    env = dict(os.environ, HOMEWIRE_SIGNING_KEY='/dev/stdin',
               HOMEWIRE_SIGNING_PASSWORD_SOURCE='env:HOMEWIRE_TV_SIGNING_PASSWORD')
    env.pop('HOMEWIRE_TV_SIGNING_KEY_B64', None)
    subprocess.run(['sh', str(Path(__file__).with_name('build.sh'))], env=env, input=key, check=True)


if __name__ == '__main__':
    main()
