#!/usr/bin/env python3
"""Package the Windows executable and dependency notices without local assets.

Reuse the Linux packager's notice collection so both platforms preserve the
licenses selected by their locked Cargo dependency graphs.
"""
import argparse
import hashlib
import json
import struct
import tempfile
import tomllib
import zipfile
from pathlib import Path

from package import PROJECT, install, write_dependency_notices

PE_OFFSET = 0x3C
PE_HEADER_BYTES = 6
PE_X86_64 = 0x8664
ZIP_COMPRESSION_LEVEL = 9


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=PROJECT/'bin/clearwave.exe')
    parser.add_argument('--output', type=Path, default=PROJECT/'releases')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)

    with binary.open('rb') as executable:
        if executable.read(2) != b'MZ':
            raise SystemExit('Expected a Windows PE executable')
        executable.seek(PE_OFFSET)
        offset = executable.read(struct.calcsize('<I'))
        if len(offset) != struct.calcsize('<I'):
            raise SystemExit('Incomplete PE header offset')
        executable.seek(struct.unpack('<I', offset)[0])
        header = executable.read(PE_HEADER_BYTES)
        if len(header) != PE_HEADER_BYTES or header[:4] != b'PE\0\0':
            raise SystemExit('Invalid PE header')
        if struct.unpack('<H', header[4:])[0] != PE_X86_64:
            raise SystemExit('Only x86-64 Windows executables are supported')

    version = tomllib.loads((PROJECT/'Cargo.toml').read_text())['package']['version']
    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output/f'clearwave-{version}-windows-x86_64.zip'

    with tempfile.TemporaryDirectory(prefix='windows-stage-', dir=args.output) as temporary:
        root = Path(temporary)/'Clearwave'
        install(binary, root/'clearwave.exe', 0o755)
        for source, destination in [
            ('README.md', 'README.md'),
            ('LICENSE', 'LICENSE'),
            ('Cargo.lock', 'Cargo.lock'),
            ('packaging/windows/BUILDING.md', 'WINDOWS.md'),
            ('ui/flags/LICENSE', 'flag-icons-LICENSE'),
            ('ui/flags/SOURCE.json', 'flag-icons-SOURCE.json'),
        ]:
            install(PROJECT/source, root/destination)
        notices = write_dependency_notices(root, 'x86_64-pc-windows-gnu')

        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED,
                             compresslevel=ZIP_COMPRESSION_LEVEL) as output:
            for file in sorted(root.rglob('*')):
                if file.is_file():
                    output.write(file, file.relative_to(root.parent))

    manifest = {
        'version': version,
        'architecture': 'x86_64',
        'target': 'x86_64-pc-windows-gnu',
        'filename': archive.name,
        'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'rust_dependency_notices': len(notices),
        'nvidia_assets_included': False,
        'style_guide_included': False,
    }
    (args.output/'windows-manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    print(json.dumps(manifest, indent=2))


if __name__ == '__main__':
    main()
