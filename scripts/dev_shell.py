#!/usr/bin/env python3
import argparse
import functools
import http.server
import os
import pathlib
import shutil
import subprocess
import sys
import threading
import time

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
CONTAINERFILE = REPO_ROOT / "tests" / "Containerfile"
MIX_BINARY = REPO_ROOT / "target" / "release" / "mix"
LOCAL_IMAGE = "mix-bootstrap-test:latest"
CACHE_DIR = (
    pathlib.Path(os.environ.get("XDG_CACHE_HOME", pathlib.Path.home() / ".cache"))
    / "mix-bootstrap-tests"
)
MIRROR_KEY_FILE = CACHE_DIR / "cache" / "mix-mirror.pub"
OFFLINE_USERS = ("ciuser", "ciuser2")
SYSTEMD_TIMEOUT = 30


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def step(message: str) -> None:
    print(f"==> {message}", flush=True)


def require(tool: str) -> None:
    if shutil.which(tool) is None:
        fail(f"`{tool}` is not installed")


def build_mix() -> None:
    step("building mix (release)")
    subprocess.run(["cargo", "build", "--release", "-p", "mix-bin"], cwd=REPO_ROOT, check=True)


def resolve_image(requested: str | None) -> str:
    image = requested or os.environ.get("MIX_TEST_IMAGE")
    if image:
        step(f"pulling {image}")
        if subprocess.run(["podman", "pull", "-q", image]).returncode == 0:
            return image
        print(f"could not pull {image}, building it locally instead", file=sys.stderr)
    step(f"building {LOCAL_IMAGE} from {CONTAINERFILE.relative_to(REPO_ROOT)}")
    subprocess.run(
        ["podman", "build", "-q", "-t", LOCAL_IMAGE, "-f", str(CONTAINERFILE), str(CONTAINERFILE.parent)],
        check=True,
    )
    return LOCAL_IMAGE


def start_container(image: str) -> str:
    name = f"mix-dev-{os.getpid()}-{time.time_ns()}"
    step(f"starting container {name}")
    subprocess.run(
        ["podman", "run", "-d", "--systemd=always", "--cap-add=SYS_ADMIN", "--name", name, image],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    deadline = time.time() + SYSTEMD_TIMEOUT
    while time.time() < deadline:
        probe = subprocess.run(
            ["podman", "exec", name, "systemctl", "is-system-running"],
            capture_output=True,
            text=True,
        )
        if probe.stdout.strip() in ("running", "degraded"):
            return name
        time.sleep(0.5)
    remove_container(name)
    fail("systemd in the container never became ready")


def remove_container(name: str) -> None:
    subprocess.run(["podman", "rm", "-f", name], capture_output=True)


def container_exec(name: str, *args: str) -> None:
    subprocess.run(["podman", "exec", name, *args], check=True, stdout=subprocess.DEVNULL)


def install_mix(name: str) -> None:
    step("copying mix into /usr/local/bin")
    subprocess.run(["podman", "cp", str(MIX_BINARY), f"{name}:/usr/local/bin/mix"], check=True)


def export_env(name: str, env: dict[str, str]) -> None:
    lines = "".join(f"export {key}='{value}'\n" for key, value in env.items())
    subprocess.run(
        ["podman", "exec", "-i", name, "bash", "-c", "cat > /etc/profile.d/mix-dev.sh"],
        input=lines,
        text=True,
        check=True,
    )


def create_user(name: str, user: str) -> None:
    step(f"creating sudo user {user}")
    container_exec(name, "useradd", "--create-home", "--shell", "/bin/bash", user)
    container_exec(
        name,
        "bash",
        "-c",
        f"echo '{user} ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/{user} && chmod 440 /etc/sudoers.d/{user}",
    )


def serve_offline_cache() -> tuple[http.server.ThreadingHTTPServer, dict[str, str]]:
    tarballs = list(CACHE_DIR.glob("nix-*.tar.xz"))
    if not tarballs or not MIRROR_KEY_FILE.exists():
        fail(
            f"no local cache in {CACHE_DIR}; run the E2E suite once (bash tests/run.sh) "
            "or drop --offline to use the internet"
        )
    handler = functools.partial(QuietHandler, directory=str(CACHE_DIR))
    server = http.server.ThreadingHTTPServer(("0.0.0.0", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://host.containers.internal:{server.server_address[1]}"
    step(f"serving the local cache at {url}")
    return server, {
        "MIX_NIX_MIRROR": url,
        "MIX_NIX_MIRROR_KEY": MIRROR_KEY_FILE.read_text().strip(),
    }


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def banner(user: str, offline: bool) -> None:
    packages = "git and hello" if offline else "any package"
    print(
        f"""
You are {user}, with sudo and no password. mix is at /usr/local/bin/mix.
Mode: {'offline, from the local E2E cache' if offline else 'online'} ({packages} can be installed).

  mix bootstrap          set everything up (asks for sudo by itself)
  mix install hello      then: hello
  mix doctor
  exit                   leave; the container is removed unless you passed --keep
""",
        flush=True,
    )


def shell(name: str, user: str, command: str | None) -> int:
    flags = ["-i", "-t"] if command is None and sys.stdin.isatty() else ["-i"]
    env_flags = ["-e", f"TERM={os.environ['TERM']}"] if os.environ.get("TERM") else []
    login = ["bash", "-l"] if command is None else ["bash", "-lc", command]
    return subprocess.run(
        ["podman", "exec", *flags, *env_flags, "-u", user, "-w", f"/home/{user}", name, *login]
    ).returncode


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build mix, start the E2E test container with it inside, and open a shell there."
    )
    parser.add_argument(
        "--offline",
        action="store_true",
        help="serve the E2E suite's local cache instead of using the internet (git and hello only)",
    )
    parser.add_argument("--user", help="user to create and log in as (default: dev, or ciuser offline)")
    parser.add_argument("--image", help="container image to use (default: $MIX_TEST_IMAGE, else built locally)")
    parser.add_argument("--no-build", action="store_true", help="use the existing target/release/mix")
    parser.add_argument("--keep", action="store_true", help="leave the container running after you exit")
    parser.add_argument("--run", metavar="COMMAND", help="run COMMAND in a login shell instead of opening one")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    require("podman")
    user = args.user or (OFFLINE_USERS[0] if args.offline else "dev")
    if args.offline and user not in OFFLINE_USERS:
        print(
            f"warning: the local cache was seeded for {' and '.join(OFFLINE_USERS)}; "
            f"setting up {user} offline would need a long build",
            file=sys.stderr,
        )

    if not args.no_build:
        require("cargo")
        build_mix()
    if not MIX_BINARY.exists():
        fail(f"{MIX_BINARY} does not exist; drop --no-build")

    server, env = serve_offline_cache() if args.offline else (None, {})
    image = resolve_image(args.image)
    name = start_container(image)
    try:
        install_mix(name)
        create_user(name, user)
        if env:
            export_env(name, env)
        if args.run is None:
            banner(user, args.offline)
        return shell(name, user, args.run)
    finally:
        if server is not None:
            server.shutdown()
        if args.keep:
            print(
                f"\nContainer kept. Re-enter with: podman exec -it -u {user} {name} bash -l\n"
                f"Remove it with: podman rm -f {name}"
            )
        else:
            remove_container(name)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        fail(f"`{' '.join(map(str, error.cmd))}` failed with exit code {error.returncode}")
    except KeyboardInterrupt:
        sys.exit(130)
