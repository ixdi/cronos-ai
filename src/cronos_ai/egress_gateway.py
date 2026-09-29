"""HTTPS CONNECT proxy enforcing an exact public-host allowlist."""

from __future__ import annotations

import ipaddress
import json
import os
import select
import socket
import socketserver
from collections.abc import Iterable
from typing import cast
from urllib.parse import urlsplit


class DestinationDenied(ValueError):
    """Raised when an outbound destination is not in the configured policy."""


def canonicalize_host(host: str) -> str:
    """Normalize a DNS hostname or IP address for exact allowlist checks."""
    try:
        return ipaddress.ip_address(host).compressed.lower()
    except ValueError:
        pass
    try:
        canonical = host.rstrip(".").encode("idna").decode("ascii").lower()
    except UnicodeError as error:
        raise DestinationDenied("invalid destination hostname") from error
    labels = canonical.split(".")
    if (
        not canonical
        or len(canonical) > 253
        or any(
            not label
            or len(label) > 63
            or not label[0].isalnum()
            or not label[-1].isalnum()
            or any(not (character.isalnum() or character == "-") for character in label)
            for label in labels
        )
    ):
        raise DestinationDenied("invalid destination hostname")
    return canonical


def authorize_connect_target(authority: str, allowed_hosts: Iterable[str]) -> str:
    """Return an allowed hostname for an HTTPS CONNECT request."""
    try:
        parsed = urlsplit(f"//{authority}")
        host = parsed.hostname
        port = parsed.port
    except ValueError as error:
        raise DestinationDenied("invalid CONNECT destination") from error
    if (
        host is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path
        or parsed.query
        or parsed.fragment
        or port != 443
    ):
        raise DestinationDenied(
            "only HTTPS CONNECT destinations on port 443 are allowed"
        )

    canonical_host = canonicalize_host(host)
    allowed = {canonicalize_host(item) for item in allowed_hosts}
    if canonical_host not in allowed:
        raise DestinationDenied("destination hostname is not allowlisted")
    return canonical_host


def extract_sni_from_client_hello(
    record: bytes,
    expected_host: str | None = None,
) -> str:
    """Extract TLS SNI and optionally require it to match CONNECT authority."""
    if len(record) < 9 or record[0] != 22 or record[1] != 3:
        raise DestinationDenied("invalid TLS ClientHello record")
    record_length = int.from_bytes(record[3:5], "big")
    if record_length > 32768 or len(record) != record_length + 5:
        raise DestinationDenied("invalid TLS ClientHello record length")

    handshake = record[5:]
    if handshake[0] != 1:
        raise DestinationDenied("TLS connection did not begin with ClientHello")
    handshake_length = int.from_bytes(handshake[1:4], "big")
    if handshake_length != len(handshake) - 4:
        raise DestinationDenied("invalid TLS ClientHello length")
    body = handshake[4:]
    offset = 34
    if len(body) <= offset:
        raise DestinationDenied("truncated TLS ClientHello")

    session_length = body[offset]
    offset += 1 + session_length
    if len(body) < offset + 2:
        raise DestinationDenied("truncated TLS cipher suites")
    cipher_length = int.from_bytes(body[offset : offset + 2], "big")
    offset += 2 + cipher_length
    if len(body) <= offset:
        raise DestinationDenied("truncated TLS compression methods")
    compression_length = body[offset]
    offset += 1 + compression_length
    if len(body) < offset + 2:
        raise DestinationDenied("truncated TLS extensions")
    extensions_length = int.from_bytes(body[offset : offset + 2], "big")
    offset += 2
    if len(body) != offset + extensions_length:
        raise DestinationDenied("invalid TLS extensions length")

    extensions_end = offset + extensions_length
    server_name: str | None = None
    while offset < extensions_end:
        if offset + 4 > extensions_end:
            raise DestinationDenied("truncated TLS extension")
        extension_type = int.from_bytes(body[offset : offset + 2], "big")
        extension_length = int.from_bytes(body[offset + 2 : offset + 4], "big")
        offset += 4
        extension_end = offset + extension_length
        if extension_end > extensions_end:
            raise DestinationDenied("invalid TLS extension length")
        if extension_type == 0:
            if server_name is not None or extension_length < 5:
                raise DestinationDenied("invalid TLS server name extension")
            names_length = int.from_bytes(body[offset : offset + 2], "big")
            names_end = offset + 2 + names_length
            if names_end != extension_end:
                raise DestinationDenied("invalid TLS server name list")
            name_offset = offset + 2
            while name_offset < names_end:
                if name_offset + 3 > names_end:
                    raise DestinationDenied("truncated TLS server name")
                name_type = body[name_offset]
                name_length = int.from_bytes(
                    body[name_offset + 1 : name_offset + 3], "big"
                )
                name_offset += 3
                name_end = name_offset + name_length
                if name_end > names_end:
                    raise DestinationDenied("invalid TLS server name length")
                if name_type == 0:
                    if server_name is not None:
                        raise DestinationDenied("duplicate TLS server name")
                    try:
                        server_name = canonicalize_host(
                            body[name_offset:name_end].decode("ascii")
                        )
                    except UnicodeDecodeError as error:
                        raise DestinationDenied("invalid TLS server name") from error
                name_offset = name_end
        offset = extension_end

    if server_name is None:
        raise DestinationDenied("TLS connection must include an SNI hostname")
    if expected_host is not None and server_name != canonicalize_host(expected_host):
        raise DestinationDenied("TLS SNI does not match the allowlisted destination")
    return server_name


def _receive_tls_record(client: socket.socket) -> bytes:
    """Read one complete bounded TLS record without trusting peer lengths."""
    header = bytearray()
    while len(header) < 5:
        chunk = client.recv(5 - len(header))
        if not chunk:
            raise DestinationDenied("TLS client closed during handshake")
        header.extend(chunk)
    record_length = int.from_bytes(header[3:5], "big")
    if header[0] != 22 or record_length > 32768:
        raise DestinationDenied("invalid TLS ClientHello record")

    record = bytearray(header)
    while len(record) < 5 + record_length:
        chunk = client.recv(5 + record_length - len(record))
        if not chunk:
            raise DestinationDenied("truncated TLS ClientHello record")
        record.extend(chunk)
    return bytes(record)


def resolve_public_addresses(host: str) -> tuple[str, ...]:
    """Resolve once and reject any non-public address to prevent SSRF/rebinding."""
    try:
        records = socket.getaddrinfo(host, 443, type=socket.SOCK_STREAM)
    except OSError as error:
        raise DestinationDenied("destination hostname could not be resolved") from error

    addresses: list[str] = []
    for record in records:
        address = record[4][0]
        if not isinstance(address, str):
            raise DestinationDenied("destination resolved to an invalid IP address")
        try:
            parsed_address = ipaddress.ip_address(address)
        except ValueError as error:
            raise DestinationDenied(
                "destination resolved to an invalid IP address"
            ) from error
        if not parsed_address.is_global:
            raise DestinationDenied("destination resolved to a non-public address")
        if address not in addresses:
            addresses.append(address)

    if not addresses:
        raise DestinationDenied("destination hostname has no public addresses")
    return tuple(addresses)


class _ProxyHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        self.request.settimeout(10)
        remote: socket.socket | None = None
        try:
            request_data = bytearray()
            while b"\r\n\r\n" not in request_data:
                chunk = self.request.recv(4096)
                if not chunk or len(request_data) + len(chunk) > 65536:
                    self._respond(400, "Invalid request")
                    return
                request_data.extend(chunk)

            headers, trailing_data = bytes(request_data).split(b"\r\n\r\n", 1)
            if trailing_data:
                self._respond(400, "Unexpected request data")
                return
            request_line = headers.split(b"\r\n", 1)[0].split()
            if (
                len(request_line) != 3
                or request_line[0] != b"CONNECT"
                or request_line[2] not in (b"HTTP/1.0", b"HTTP/1.1")
            ):
                self._respond(405, "CONNECT required")
                return

            try:
                target = request_line[1].decode("ascii")
                proxy_server = cast(EgressProxyServer, self.server)
                host = authorize_connect_target(target, proxy_server.allowed_hosts)
                addresses = resolve_public_addresses(host)
            except (UnicodeDecodeError, DestinationDenied):
                self._respond(403, "Destination denied")
                return

            self.request.sendall(
                b"HTTP/1.1 200 Connection Established\r\n\r\n"
            )
            client_hello = _receive_tls_record(self.request)
            extract_sni_from_client_hello(client_hello, host)

            for address in addresses:
                try:
                    remote = socket.create_connection((address, 443), timeout=10)
                    break
                except OSError:
                    continue
            if remote is None:
                return

            self.request.settimeout(None)
            remote.settimeout(None)
            remote.sendall(client_hello)
            self._relay(self.request, remote)
        except (DestinationDenied, OSError, TimeoutError):
            return
        finally:
            if remote is not None:
                remote.close()

    def _respond(self, status: int, reason: str) -> None:
        response = (
            f"HTTP/1.1 {status} {reason}\r\n"
            "Connection: close\r\n"
            "Content-Length: 0\r\n\r\n"
        )
        self.request.sendall(response.encode("ascii"))

    @staticmethod
    def _relay(client: socket.socket, remote: socket.socket) -> None:
        sockets = (client, remote)
        while True:
            readable, _, exceptional = select.select(sockets, (), sockets, 300)
            if exceptional or not readable:
                return
            for source in readable:
                destination = remote if source is client else client
                data = source.recv(65536)
                if not data:
                    return
                destination.sendall(data)


class EgressProxyServer(socketserver.ThreadingTCPServer):
    """Threaded local HTTPS proxy configured with allowed DNS hostnames."""

    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        allowed_hosts: Iterable[str],
    ) -> None:
        self.allowed_hosts = frozenset(
            canonicalize_host(host) for host in allowed_hosts
        )
        super().__init__(address, _ProxyHandler)


def main() -> None:
    """Run the sidecar proxy from its Docker container environment."""
    allowed_hosts = json.loads(os.environ.get("FACTORY_EGRESS_ALLOWLIST", "[]"))
    if not isinstance(allowed_hosts, list) or not all(
        isinstance(host, str) for host in allowed_hosts
    ):
        raise ValueError("invalid egress allowlist configuration")
    with EgressProxyServer(("127.0.0.1", 3128), allowed_hosts) as server:
        server.serve_forever(poll_interval=0.2)


if __name__ == "__main__":
    main()
