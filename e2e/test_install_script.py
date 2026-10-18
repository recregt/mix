import contextlib
import functools
import hashlib
import http.server
import io
import platform
import subprocess
import tarfile
import threading

import pytest

from support.container import REPO_ROOT, create_user

SCRIPT = REPO_ROOT / "scripts/install.sh"
IN_CONTAINER = "/tmp/install.sh"
USER = "installer"
ARCH = {"x86_64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}[platform.machine()]
ARCHIVE = f"mix-{ARCH}-linux.tar.gz"


class _Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def _archive(mix_binary) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        for program in (mix_binary, mix_binary.with_name("mix-daemon")):
            archive.add(program, arcname=f"mix-{ARCH}-linux/{program.name}")
    return buffer.getvalue()


@contextlib.contextmanager
def _release(directory, mix_binary, checksum: str | None = None):
    archive = _archive(mix_binary)
    (directory / ARCHIVE).write_bytes(archive)
    digest = checksum or hashlib.sha256(archive).hexdigest()
    (directory / f"{ARCHIVE}.sha256").write_text(f"{digest}  {ARCHIVE}\n")
    handler = functools.partial(_Quiet, directory=str(directory))
    server = http.server.ThreadingHTTPServer(("0.0.0.0", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        yield f"http://host.containers.internal:{server.server_address[1]}"
    finally:
        server.shutdown()


def _copy_script(container):
    subprocess.run(["podman", "cp", str(SCRIPT), f"{container.name}:{IN_CONTAINER}"], check=True)
    container.exec("chmod", "0755", IN_CONTAINER, check=True)


def _installed(container) -> bool:
    return container.exec("test", "-e", "/usr/local/bin/mix").returncode == 0


@pytest.mark.unmounted
def test_the_installer_puts_both_programs_on_the_path_through_sudo(container, mix_binary, tmp_path):
    create_user(container, USER, sudo=True)
    _copy_script(container)

    with _release(tmp_path, mix_binary) as url:
        run = container.exec("sh", IN_CONTAINER, env={"MIX_DOWNLOAD_URL": url}, user=USER)

    assert run.returncode == 0, run
    assert "mix bootstrap --dry-run" in run.stdout, run
    for program in (mix_binary, mix_binary.with_name("mix-daemon")):
        installed = f"/usr/local/bin/{program.name}"
        found = container.exec("sha256sum", installed, check=True).stdout.split()[0]
        assert found == hashlib.sha256(program.read_bytes()).hexdigest(), installed
        owner = container.exec("stat", "-c", "%U %a", installed, check=True).stdout.strip()
        assert owner == "root 755", installed
    version = container.exec("mix", "--version", user=USER, check=True).stdout
    assert version.startswith("mix "), version


@pytest.mark.unmounted
def test_an_archive_that_does_not_match_its_checksum_installs_nothing(
    container, mix_binary, tmp_path
):
    _copy_script(container)

    with _release(tmp_path, mix_binary, checksum="0" * 64) as url:
        run = container.exec("sh", IN_CONTAINER, env={"MIX_DOWNLOAD_URL": url})

    assert run.returncode != 0, run
    assert "does not match its published checksum" in run.stderr, run
    assert not _installed(container)


@pytest.mark.unmounted
def test_without_curl_or_wget_the_installer_says_so_and_installs_nothing(
    container, mix_binary, tmp_path
):
    _copy_script(container)
    container.exec("dpkg", "--remove", "curl", check=True)

    with _release(tmp_path, mix_binary) as url:
        run = container.exec("sh", IN_CONTAINER, env={"MIX_DOWNLOAD_URL": url})

    assert run.returncode != 0, run
    assert "needs curl or wget" in run.stderr, run
    assert not _installed(container)
