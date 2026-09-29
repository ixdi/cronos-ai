import socket
import threading

import pytest

from cronos_ai.egress_gateway import (
    DestinationDenied,
    EgressProxyServer,
    authorize_connect_target,
    extract_sni_from_client_hello,
    resolve_public_addresses,
)


def client_hello(host: str) -> bytes:
    host_bytes = host.encode("ascii")
    server_name = b"\x00" + len(host_bytes).to_bytes(2, "big") + host_bytes
    server_names = len(server_name).to_bytes(2, "big") + server_name
    extension = b"\x00\x00" + len(server_names).to_bytes(2, "big") + server_names
    extensions = len(extension).to_bytes(2, "big") + extension
    body = (
        b"\x03\x03"
        + b"\x00" * 32
        + b"\x00"
        + b"\x00\x02\x13\x01"
        + b"\x01\x00"
        + extensions
    )
    handshake = b"\x01" + len(body).to_bytes(3, "big") + body
    return b"\x16\x03\x01" + len(handshake).to_bytes(2, "big") + handshake


def test_connect_authorization_allows_only_exact_allowlisted_https_hosts() -> None:
    assert authorize_connect_target(
        "API.Provider.Example:443",
        {"api.provider.example"},
    ) == "api.provider.example"

    with pytest.raises(DestinationDenied):
        authorize_connect_target(
            "attacker.example:443",
            {"api.provider.example"},
        )


def test_connect_authorization_rejects_non_https_ports_and_userinfo() -> None:
    allowed = {"api.provider.example"}

    with pytest.raises(DestinationDenied):
        authorize_connect_target("api.provider.example:80", allowed)
    with pytest.raises(DestinationDenied):
        authorize_connect_target("user@api.provider.example:443", allowed)
    with pytest.raises(DestinationDenied):
        authorize_connect_target("api.provider.example:443/path", allowed)


def test_tls_client_hello_sni_must_match_connect_host() -> None:
    record = client_hello("api.provider.example")

    assert extract_sni_from_client_hello(record) == "api.provider.example"
    with pytest.raises(DestinationDenied, match="SNI"):
        extract_sni_from_client_hello(
            client_hello("other.example"),
            "api.provider.example",
        )


def test_tls_client_hello_without_sni_is_rejected() -> None:
    with pytest.raises(DestinationDenied):
        extract_sni_from_client_hello(b"\x16\x03\x01\x00\x00")


def test_live_proxy_rejects_unallowlisted_and_private_destinations() -> None:
    with EgressProxyServer(("127.0.0.1", 0), {"localhost"}) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            for target in ("attacker.example:443", "localhost:443"):
                with socket.create_connection(
                    server.server_address,
                    timeout=2,
                ) as client:
                    client.sendall(
                        f"CONNECT {target} HTTP/1.1\r\n"
                        f"Host: {target}\r\n\r\n".encode("ascii")
                    )
                    response = client.recv(1024)
                    assert b"403" in response
        finally:
            server.shutdown()
            thread.join(timeout=2)


def test_dns_resolution_rejects_private_or_mixed_public_private_addresses(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def private_lookup(*args: object, **kwargs: object) -> list[tuple[object, ...]]:
        return [
            (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", 443)),
        ]

    monkeypatch.setattr(socket, "getaddrinfo", private_lookup)
    with pytest.raises(DestinationDenied, match="non-public address"):
        resolve_public_addresses("api.provider.example")


def test_dns_resolution_returns_public_addresses_without_re_resolving(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def public_lookup(*args: object, **kwargs: object) -> list[tuple[object, ...]]:
        return [
            (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("8.8.8.8", 443)),
        ]

    monkeypatch.setattr(socket, "getaddrinfo", public_lookup)

    assert resolve_public_addresses("api.provider.example") == ("8.8.8.8",)
