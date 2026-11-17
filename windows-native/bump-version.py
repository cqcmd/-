#!/usr/bin/env python3
"""版本号递增工具：严格执行“3段式+日期，每段满10进位”规则，并同步 build_version.txt 与 Cargo.toml"""

import datetime
import re
from pathlib import Path

ROOT = Path(__file__).parent
CARGO_TOML = ROOT / "Cargo.toml"
VERSION_FILE = ROOT / "build_version.txt"


def bump_base10_version(version_str: str, today: str) -> str:
    m = re.match(r"^(\d+)\.(\d+)\.(\d+)\+(\d{8})$", version_str.strip())
    if not m:
        raise ValueError(f"无法识别的版本号格式: {version_str}")
    major, minor, patch = int(m.group(1)), int(m.group(2)), int(m.group(3))

    patch += 1
    if patch >= 10:
        patch = 0
        minor += 1
        if minor >= 10:
            minor = 0
            major += 1

    return f"{major}.{minor}.{patch}+{today}"


def main() -> None:
    today = datetime.date.today().strftime("%Y%m%d")
    current_source = (
        VERSION_FILE.read_text(encoding="utf-8").strip()
        if VERSION_FILE.exists()
        else ""
    )

    content = CARGO_TOML.read_text(encoding="utf-8")
    if not current_source:
        m = re.search(r'^version\s*=\s*"([^"]+)"', content, flags=re.MULTILINE)
        if not m:
            raise RuntimeError("未在 Cargo.toml 中找到 version 字段")
        current_source = m.group(1)

    new_ver = bump_base10_version(current_source, today)
    print(f"版本递增: {current_source} -> {new_ver}")

    VERSION_FILE.write_text(f"{new_ver}\n", encoding="utf-8")
    updated, count = re.subn(
        r'^version\s*=\s*"([^"]+)"',
        f'version = "{new_ver}"',
        content,
        count=1,
        flags=re.MULTILINE,
    )
    if count == 1:
        CARGO_TOML.write_text(updated, encoding="utf-8")


if __name__ == "__main__":
    main()
