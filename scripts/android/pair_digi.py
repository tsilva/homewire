#!/usr/bin/env python3
"""Pair Android TV Remote v2 using its displayed PIN (host setup only).
Python standard library + OpenSSL. Runtime Homewire needs neither. Credentials
stay in a private directory; existing files are never overwritten.
"""
import argparse
import getpass
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess


def varint(n):
    result = bytearray()
    while n > 127:
        result.append((n & 127) | 128)
        n >>= 7
    return bytes(result + bytes([n]))


def integer(field, value):
    return varint(field * 8) + varint(value)


def nested(field, value):
    return varint(field * 8 + 2) + varint(len(value)) + value


def decode(data):
    pos = 0
    def read():
        nonlocal pos
        n = 0
        for shift in range(0, 70, 7):
            if pos >= len(data):
                raise ValueError('Truncated pairing message')
            b = data[pos]
            pos += 1
            n |= (b & 127) << shift
            if b < 128:
                return n
        raise ValueError('Invalid varint')
    fields = {}
    while pos < len(data):
        tag = read()
        if tag & 7 == 0:
            value = read()
        elif tag & 7 == 2:
            size = read()
            if size > len(data) - pos:
                raise ValueError('Truncated field')
            value = data[pos:pos + size]
            pos += size
        else:
            raise ValueError('Unsupported pairing wire type')
        if tag >> 3 in fields:
            raise ValueError('Duplicate pairing field')
        fields[tag >> 3] = value
    return fields


def exact(stream, size):
    data = bytearray()
    while len(data) < size:
        b = stream.recv(size - len(data))
        if not b:
            raise ValueError('Pairing connection closed')
        data.extend(b)
    return bytes(data)


def receive(stream, expected):
    size = 0
    for shift in range(0, 35, 7):
        b = exact(stream, 1)[0]
        size |= (b & 127) << shift
        if b < 128:
            break
    else:
        raise ValueError('Invalid frame length')
    if not 1 <= size <= 8192:
        raise ValueError('Pairing frame exceeds bound')
    fields = decode(exact(stream, size))
    if fields.get(2) != 200 or expected not in fields:
        raise ValueError('Pairing request rejected or unexpected response')
    return fields


def send(stream, field, body):
    data = integer(1, 2) + integer(2, 200) + nested(field, body)
    stream.sendall(varint(len(data)) + data)


def public_numbers(cert_der):
    pub = subprocess.run(['openssl', 'x509', '-inform', 'DER', '-pubkey', '-noout'],
                         input=cert_der, capture_output=True, check=True).stdout
    text = subprocess.run(['openssl', 'pkey', '-pubin', '-text', '-noout'],
                          input=pub, capture_output=True, check=True).stdout.decode()
    match = re.search(r'Modulus:\s*(.*?)\s*Exponent:\s*(\d+)', text, re.S)
    if not match:
        raise ValueError('Android pairing requires RSA certificates')
    modulus = bytes.fromhex(re.sub(r'\s|:', '', match[1])).lstrip(b'\0')
    exponent = int(match[2])
    return modulus, exponent.to_bytes((exponent.bit_length() + 7) // 8, 'big')


def secret(client, server, code):
    if not re.fullmatch('[0-9a-fA-F]{6}', code):
        raise ValueError('Expected six hexadecimal characters')
    cmod, cexp = public_numbers(client)
    smod, sexp = public_numbers(server)
    digest = hashlib.sha256(cmod + cexp + smod + sexp + bytes.fromhex(code[2:])).digest()
    if digest[0] != int(code[:2], 16):
        raise ValueError('PIN does not authenticate the pairing certificates')
    return digest


def pair(host, directory):
    ipaddress.ip_address(host)
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    cert, key = directory / 'digi-cert.pem', directory / 'digi-key.pem'
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                    '-keyout', str(key), '-out', str(cert), '-days', '3650',
                    '-subj', '/CN=Homewire'], check=True, capture_output=True)
    os.chmod(key, 0o600)
    os.chmod(cert, 0o600)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE  # PIN authenticates this first connection.
    context.load_cert_chain(cert, key)
    with socket.create_connection((host, 6467), timeout=10) as tcp:
        with context.wrap_socket(tcp, server_hostname=host) as stream:
            server = stream.getpeercert(binary_form=True)
            client = ssl.PEM_cert_to_DER_cert(cert.read_text())
            send(stream, 10, nested(1, b'atvremote') + nested(2, b'Homewire'))
            receive(stream, 11)
            encoding = integer(1, 3) + integer(2, 6)
            send(stream, 20, nested(1, encoding) + integer(3, 1))
            receive(stream, 20)
            send(stream, 30, nested(1, encoding) + integer(2, 1))
            receive(stream, 31)
            code = getpass.getpass('Enter the six-character PIN displayed on DIGI: ')
            send(stream, 40, nested(1, secret(client, server, code)))
            receive(stream, 41)
    with socket.create_connection((host, 6466), timeout=10) as tcp:
        with context.wrap_socket(tcp, server_hostname=host) as stream:
            remote = stream.getpeercert(binary_form=True)
            if public_numbers(remote) != public_numbers(server):
                raise ValueError('Remote and pairing servers have different identities')
    config = {'host': host, 'server_cert_sha256': hashlib.sha256(remote).hexdigest(),
              'client_cert': cert.name, 'client_key': key.name}
    fd = os.open(directory / 'digi.json', os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, 'w') as output:
        json.dump(config, output, indent=2)
        output.write('\n')
    print('Paired and saved private DIGI configuration. Validate digi-status before deployment.')


if __name__ == '__main__':
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--host', required=True)
    ap.add_argument('--output-directory', type=Path, required=True)
    args = ap.parse_args()
    pair(args.host, args.output_directory)
