"""Read embedded identity without executing a possibly foreign-architecture binary."""
import mmap
from pathlib import Path
import re
import sys

PREFIX = b"\0HERDR_BUILD_IDENTITY_V1\n"
RECORD = re.compile(rb"worktree=([01])\nbranch=([^\x00-\x1f\x7f]*)\npr=([0-9]*)\n")


def identity(path):
    with open(path, "rb") as binary:
        with mmap.mmap(binary.fileno(), 0, access=mmap.ACCESS_READ) as data:
            records = set()
            start = data.find(PREFIX)
            while start != -1:
                body = start + len(PREFIX)
                end = data.find(b"\0", body, body + 4096)
                match = RECORD.fullmatch(data[body:end]) if end != -1 else None
                if not match:
                    raise ValueError(f"Malformed build identity: {path}")
                worktree, branch, pr = match.groups()
                if (worktree == b"0" and branch) or (worktree == b"1" and not branch) or (pr and not pr.strip(b"0")):
                    raise ValueError(f"Invalid build identity: {path}")
                records.add(match.groups())
                start = data.find(PREFIX, end + 1)
            if len(records) != 1:
                raise ValueError(f"Missing or conflicting build identity: {path}")
            return records.pop()


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("png", "icns"):
        raise ValueError("Usage: python3 build-icon.py png|icns BINARY [BINARY ...]")
    identities = {identity(path) for path in sys.argv[2:]}
    if len(identities) != 1:
        raise ValueError("Input binaries have different build identities")
    worktree, _, _ = identities.pop()
    suffix = "-worktree" if worktree == b"1" else ""
    name = ("herdr-square-worktree-1024.png" if worktree == b"1" else "herdr-icon-square-clean.png") if sys.argv[1] == "png" else f"Herdr{suffix}.icns"
    print(Path(__file__).resolve().parents[2] / "assets/icons" / name)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        sys.exit(str(error))
