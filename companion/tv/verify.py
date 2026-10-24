"""Bounded read-only HTTPS acceptance checks. Pin TLS before sending any token."""
import hashlib
import http.client
import io
import ipaddress
import json
import os
import re
import socket
import ssl
import time


class IdentityError(ValueError):
    pass


def request(host, port, pin, token=None, connect=None):
    address = connect or (host, port)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE  # Exact certificate pin is checked below.
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    with socket.create_connection(address, timeout=3) as tcp:
        with context.wrap_socket(tcp, server_hostname='homewire-tv') as tls:
            if hashlib.sha256(tls.getpeercert(binary_form=True)).hexdigest() != pin:
                raise IdentityError('TV HTTPS certificate changed; no credentials sent')
            headers = 'GET /v1/status HTTP/1.1\r\nHost: homewire-tv\r\nConnection: close\r\n'
            if token is not None:
                headers += 'Authorization: Bearer ' + token + '\r\n'
            tls.sendall((headers + '\r\n').encode('ascii'))
            raw = bytearray()
            deadline = time.monotonic() + 5
            while b'\r\n\r\n' not in raw:
                if len(raw) >= 4096 or time.monotonic() >= deadline:
                    raise ValueError('TV HTTPS headers exceeded limit')
                chunk = tls.recv(1)
                if not chunk:
                    raise ValueError('TV HTTPS response truncated')
                raw.extend(chunk)

            class ResponseSocket:
                def makefile(self, *args, **kwargs):
                    return io.BytesIO(bytes(raw))

            response = http.client.HTTPResponse(ResponseSocket())
            response.begin()
            length = response.getheader('Content-Length', '')
            if not re.fullmatch(r'0|[1-9][0-9]{0,3}', length) or int(length) > 4096 or response.getheader('Transfer-Encoding'):
                raise ValueError('TV HTTPS response framing invalid')
            body = bytearray()
            while len(body) < int(length):
                if time.monotonic() >= deadline:
                    raise ValueError('TV HTTPS response deadline exceeded')
                chunk = tls.recv(int(length) - len(body))
                if not chunk:
                    raise ValueError('TV HTTPS body truncated')
                body.extend(chunk)
            return response.status, json.loads(body)


def verify(profile, config, connect=None, wait_seconds=20):
    ipaddress.ip_address(profile['host'])
    pin = config['server_cert_sha256']
    token = os.environ.get('HOMEWIRE_SECRET_TV_API_TOKEN', '')
    if not re.fullmatch('[a-f0-9]{64}', pin) or not re.fullmatch('[a-f0-9]{64}', token):
        raise ValueError('Invalid certificate pin or missing runtime credential')
    until = time.monotonic() + wait_seconds
    while True:
        try:
            code, status = request(profile['host'], profile['port'], pin, token, connect)
            break
        except (OSError, TimeoutError):
            if time.monotonic() >= until:
                raise RuntimeError('TV HTTPS unavailable; update is unverified') from None
            time.sleep(0.5)  # Reads only; never replay an install/control.
    expected = dict(protocol_version=1, manufacturer='TCL', **{k: profile[k] for k in ('model', 'device', 'fingerprint', 'incremental')})
    if code != 200 or not isinstance(status, dict) or any(status.get(k) != v for k, v in expected.items()):
        raise IdentityError('TV HTTPS authentication or firmware verification failed')
    level = status.get('backlight_level')
    if type(level) is not int or not 0 <= level <= 100:
        raise ValueError('Invalid native backlight status')
    for credential in (None, '0' * 64 if token != '0' * 64 else '1' * 64):
        if request(profile['host'], profile['port'], pin, credential, connect)[0] != 401:
            raise ValueError('TV HTTPS authentication rejection check failed')
    return {'pinned_https': True, 'authenticated_status': True,
            'unauthenticated_rejected': True, 'bad_token_rejected': True,
            'backlight_level': level}
