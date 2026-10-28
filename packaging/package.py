#!/usr/bin/env python3
"""Assemble model-free amd64 packages from a verified compatible executable."""
import argparse, hashlib, json, os, re, shutil, subprocess, tempfile, tomllib
from pathlib import Path

PROJECT = Path(__file__).resolve().parents[1]
TARGETS = {'debian13': 'Debian 13', 'ubuntu24.04': 'Ubuntu 24.04 LTS and newer', 'mint22': 'Linux Mint 22.x'}
DEPENDENCIES = ', '.join([
    'libc6 (>= 2.39)', 'libgcc-s1 (>= 4.2)', 'libgtk-3-0t64 (>= 3.24.41)',
    'libglib2.0-0t64 (>= 2.80)', 'libgdk-pixbuf-2.0-0 (>= 2.42)',
    'libpango-1.0-0 (>= 1.52)', 'librsvg2-common', 'libvulkan1',
    'pipewire-bin (>= 1.0)', 'wireplumber (>= 0.4)', 'coreutils', 'util-linux',
    'adwaita-icon-theme',
])

def install(source, destination, mode=0o644):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)

def write_dependency_notices(doc, platform):
    metadata = json.loads(subprocess.check_output([
        'cargo', 'metadata', '--locked', '--offline', '--format-version', '1',
        '--filter-platform', platform,
    ], cwd=PROJECT))
    licenses = []
    for package in metadata['packages']:
        if package['name'] == 'clearwave': continue
        source = Path(package['manifest_path']).parent
        candidates = {f for f in source.iterdir() if f.is_file() and
            f.name.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT'))}
        if package.get('license_file'):
            candidates.add(source/package['license_file'])
        for folder in ['LICENSES', 'licenses']:
            if (source/folder).is_dir(): candidates.update(f for f in (source/folder).rglob('*') if f.is_file())
        destination = doc/'dependency-licenses'/f"{package['name']}-{package['version']}"
        for file in sorted(candidates):
            install(file, destination/file.relative_to(source))
        fallback = None
        if not candidates:
            if 'Apache-2.0' in (package['license'] or ''):
                fallback = 'Apache-2.0'
            elif package['license'] == 'CC0-1.0':
                fallback = 'CC0-1.0'
            else:
                raise SystemExit(f"Missing licence notice for {package['name']}")
            install(PROJECT/'packaging/licenses'/fallback, destination/'LICENSE')
        destination.mkdir(parents=True, exist_ok=True)
        (destination/'PACKAGE.json').write_text(json.dumps({
            'name': package['name'], 'version': package['version'],
            'authors': package['authors'], 'repository': package['repository'],
            'declared_license': package['license'],
            'selected_license_when_no_packaged_notice': fallback,
        }, indent=2)+'\n')
        licenses.append({'name': package['name'], 'version': package['version'], 'license': package['license'], 'notice_files': len(candidates), 'fallback_license_text': fallback})
    (doc/'dependency-licenses.json').write_text(json.dumps(licenses, indent=2)+'\n')
    return licenses

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=PROJECT/'bin/clearwave')
    parser.add_argument('--output', type=Path, default=PROJECT/'releases')
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    version = tomllib.loads((PROJECT/'Cargo.toml').read_text())['package']['version']
    header = subprocess.check_output(['readelf', '-h', binary], text=True)
    if 'Advanced Micro Devices X86-64' not in header:
        raise SystemExit('Only x86-64 ELF executables are supported')
    versions = subprocess.check_output(['readelf', '--version-info', binary], text=True)
    glibc = sorted({tuple(map(int, x.split('.'))) for x in re.findall(r'GLIBC_([0-9.]+)', versions)})
    if not glibc or glibc[-1] > (2, 39):
        raise SystemExit('Rebuild against the Ubuntu 24.04 sysroot; executable requires newer glibc')
    dynamic = subprocess.check_output(['readelf', '-d', binary], text=True)
    if '(RPATH)' in dynamic or '(RUNPATH)' in dynamic:
        raise SystemExit('Release executable must not contain a build-machine library path')
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {'version': version, 'architecture': 'amd64', 'build_baseline': 'Ubuntu 24.04 amd64 sysroot',
        'maximum_glibc_symbol': '.'.join(map(str, glibc[-1])),
        'nvidia_assets_included': False, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'packages': []}
    with tempfile.TemporaryDirectory(prefix='deb-stage-', dir=args.output) as temporary:
        base = Path(temporary)/'base'
        install(binary, base/'usr/bin/clearwave', 0o755)
        install(PROJECT/'packaging/clearwave.desktop', base/'usr/share/applications/clearwave.desktop')
        install(PROJECT/'packaging/clearwave.svg', base/'usr/share/icons/hicolor/scalable/apps/clearwave.svg')
        doc = base/'usr/share/doc/clearwave'
        install(PROJECT/'README.md', doc/'README.md')
        install(PROJECT/'docs/linux-frontend.png', doc/'docs/linux-frontend.png')
        install(PROJECT/'docs/BROADCAST_MODEL.md', doc/'docs/BROADCAST_MODEL.md')
        install(PROJECT/'docs/broadcast-comparison.json', doc/'docs/broadcast-comparison.json')
        install(PROJECT/'docs/ECHO_REDUCTION.md', doc/'ECHO_REDUCTION.md')
        install(PROJECT/'LICENSE', doc/'LICENSE')
        install(PROJECT/'Cargo.lock', doc/'Cargo.lock')
        install(PROJECT/'ui/flags/LICENSE', doc/'flag-icons-LICENSE')
        install(PROJECT/'ui/flags/SOURCE.json', doc/'flag-icons-SOURCE.json')
        install(PROJECT/'packaging/README.Debian', doc/'README.Debian')
        (doc/'copyright').write_text(
            'Clearwave — local application build\n\n'
            'Copyright (c) 2026 NullMagic2 and Clearwave contributors.\n'
            'Original application code: MIT; see LICENSE.\n'
            'Third-party Rust components retain their respective licences.\n'
            'See dependency-licenses/ for their notices and Cargo.lock for exact versions.\n'
            'Country flag artwork: flag-icons, Copyright (c) 2013 Panayiotis Lipiridis, MIT.\n'
            'See flag-icons-LICENSE and flag-icons-SOURCE.json.\n'
            'No NVIDIA installer, DLL, model package or exported weights are included.\n')
        write_dependency_notices(doc, 'x86_64-unknown-linux-gnu')
        for target, label in TARGETS.items():
            root = Path(temporary)/target
            shutil.copytree(base, root)
            control = root/'DEBIAN'; control.mkdir()
            installed_size = (sum(f.stat().st_size for f in root.rglob('*') if f.is_file())+1023)//1024
            package_version = version+'-1+'+target
            (control/'control').write_text(
                f'Package: clearwave\nVersion: {package_version}\nArchitecture: amd64\n'
                'Section: sound\nPriority: optional\nMaintainer: Local application build <noreply@localhost>\n'
                f'Installed-Size: {installed_size}\nDepends: {DEPENDENCIES}\n'
                'Conflicts: rtx-voice-amd\nReplaces: rtx-voice-amd\n'
                'Recommends: mesa-vulkan-drivers\nSuggests: 7zip | p7zip-full, curl\n'
                f'Description: Native microphone denoising controls for {label}\n'
                ' GTK3 interface with AMD Vulkan processing and a PipeWire virtual microphone.\n'
                ' NVIDIA model and runtime files are not included; provide a local model bundle.\n')
            sums = ''.join(f'{hashlib.md5(f.read_bytes()).hexdigest()}  {f.relative_to(root)}\n'
                for f in sorted(root.rglob('*')) if f.is_file() and 'DEBIAN' not in f.relative_to(root).parts)
            (control/'md5sums').write_text(sums)
            name = f'clearwave_{package_version}_amd64.deb'
            subprocess.run(['dpkg-deb', '--root-owner-group', '-Zxz', '--build', root, args.output/name], check=True)
            manifest['packages'].append({'filename': name, 'target': label,
                'sha256': hashlib.sha256((args.output/name).read_bytes()).hexdigest()})
    (args.output/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    (args.output/'SHA256SUMS').write_text(''.join(f"{p['sha256']}  {p['filename']}\n" for p in manifest['packages']))
    install(PROJECT/'packaging/INSTALL.md', args.output/'INSTALL.md')
    print(json.dumps(manifest, indent=2))

if __name__ == '__main__': main()
