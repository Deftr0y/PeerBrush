"""Read exact public APT objects without redirects, credentials or local writes."""
import hashlib
import http.client
import ipaddress
import socket
import ssl
import time

from attach_apt_signatures import repository_origin
import prepare_apt_repository as apt


def addresses(host):
    try:
        answers = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM, proto=socket.IPPROTO_TCP)
    except OSError:
        raise ValueError('The approved APT host did not resolve.') from None
    endpoints = []
    for family, kind, protocol, _, endpoint in answers:
        address = ipaddress.ip_address(endpoint[0])
        if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped is not None:
            address = address.ipv4_mapped
        if (family not in (socket.AF_INET, socket.AF_INET6) or not address.is_global
                or address.is_multicast or address.is_reserved or getattr(address, 'is_site_local', False)):
            raise ValueError('The approved APT host must resolve only to public unicast addresses.')
        endpoints.append((family, kind, protocol, endpoint))
    if not endpoints:
        raise ValueError('The approved APT host has no public HTTPS address.')
    return endpoints


def connect(host, endpoints, context, deadline):
    for family, kind, protocol, endpoint in endpoints:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        try:
            with socket.socket(family, kind, protocol) as raw:
                raw.settimeout(min(15, remaining))
                raw.connect(endpoint)
                return context.wrap_socket(raw, server_hostname=host)
        except OSError:
            continue
    raise ValueError('The approved APT host failed verified TLS connection.')


def compare(origin, endpoints, context, path, record):
    expected = record['bytes']
    checksum = hashlib.sha256()
    deadline = time.monotonic() + 120
    connection = http.client.HTTPSConnection(origin.hostname, timeout=15, context=context)
    try:
        # Connect only to the checked DNS answers while retaining hostname validation and SNI.
        connection.sock = connect(origin.hostname, endpoints, context, deadline)
        connection.request('GET', origin.path.rstrip('/') + '/' + path, headers={
            'User-Agent': 'PeerBrush-APT-readback', 'Accept-Encoding': 'identity',
            'Cache-Control': 'no-cache', 'Pragma': 'no-cache', 'Connection': 'close',
        })
        with connection.getresponse() as response:
            if response.status != 200:
                raise ValueError('Public APT readback requires HTTP 200 at the selected URL.')
            lengths = response.headers.get_all('Content-Length', [])
            encodings = response.headers.get_all('Content-Encoding', [])
            framing = response.headers.get_all('Transfer-Encoding', [])
            if (lengths not in ([], [str(expected)]) or encodings not in ([], ['identity'])
                    or framing not in ([], ['chunked']) or (framing and lengths)
                    or response.headers.get_all('Content-Range', [])):
                raise ValueError('Public APT response has unexpected framing, encoding or size.')
            size = 0
            while True:
                if time.monotonic() >= deadline:
                    raise ValueError('Public APT transfer exceeded its deadline.')
                chunk = response.read1(min(64 * 1024, expected + 1 - size))
                if time.monotonic() >= deadline:
                    raise ValueError('Public APT transfer exceeded its deadline.')
                if not chunk:
                    break
                size += len(chunk)
                if size > expected:
                    raise ValueError('Public APT object exceeds its recorded size.')
                checksum.update(chunk)
            if size != expected or checksum.hexdigest() != record['sha256']:
                raise ValueError('Public APT object differs from the signed candidate receipt.')
    except (OSError, http.client.HTTPException):
        raise ValueError('Public APT GET failed; no readback evidence was produced.') from None
    finally:
        connection.close()


def check(repository_uri, bootstrap_uri, files, bootstrap):
    repository = repository_origin(repository_uri)
    client = repository_origin(bootstrap_uri)
    context = ssl.create_default_context()
    repo_addresses = addresses(repository.hostname)
    client_addresses = repo_addresses if repository.hostname == client.hostname else addresses(client.hostname)
    inrelease = apt.DIST + 'InRelease'
    # Bracket the longer payload reads with the same signed repository metadata.
    compare(repository, repo_addresses, context, inrelease, files[inrelease])
    for path, record in files.items():
        if path != inrelease:
            compare(repository, repo_addresses, context, path, record)
    for path, record in bootstrap.items():
        compare(client, client_addresses, context, path.removeprefix('bootstrap/'), record)
    compare(repository, repo_addresses, context, inrelease, files[inrelease])
