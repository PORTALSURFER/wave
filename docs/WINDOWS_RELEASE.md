# WAVE Windows release artifact

`.github/workflows/windows-release.yml` is WAVE's unsigned Windows packaging
lane. It is called by the nightly release and release-preflight workflows (and
can be run standalone for inspection). The lane checks out the exact source SHA supplied
by the prepare job, builds only `x86_64-pc-windows-msvc`, and receives no Apple,
PortalSurfer, or OIDC publishing credential.

The pinned build inputs are Windows Server 2022, Rust `1.97.1`, Toybox
`a69df15593a5cb9320993dde8d9908bfe857a9f6`, Radiant
`c1343993c973bdece3e8cd469415b0d08c7f6cf1`, and the VST3 SDK
`58f8da7936800732561402d7936584ca4505de07`. The standard VST3 bundle is
emitted at:

```text
dist/WAVE-v<package-version>.vst3/Contents/x86_64-win/WAVE-v<package-version>.vst3
```

The packaging helper validates the AMD64 PE32+ headers and rejects any
Authenticode security directory. It emits exactly these adjacent files:

```text
wave-v<publication-version>-windows-x86_64-unsigned.vst3.zip
windows-artifact-manifest.json
```

The ZIP contains one regular member with this forward-slash topology:

```text
WAVE-v<package-version>.vst3/Contents/x86_64-win/WAVE-v<package-version>.vst3
```

The schema-1 sidecar records the immutable source SHA, package/publication
identity, exact Toybox/Radiant/VST3 SDK revisions, runner image and version,
Rust target/toolchain/compiler, CPython version, archive/member hashes and
layout, and explicit unsigned/no-certificate provenance. It is canonical JSON,
uses deterministic ZIP metadata, refuses overwrite, and is validated again by
the final macOS assembly job.

Nightly prepare computes one source SHA, publication version, build ID, and
timestamp. The reusable Windows job receives those exact values. The final
macOS job validates the sidecar and assembles the macOS arm64 CLAP, macOS arm64
VST3, and Windows x86_64 VST3 archives into one schema-3 nightly manifest.
The macOS entries retain WAVE's Developer ID team identity; the Windows entry
remains unsigned. Stable and RC continue to use the schema-2 macOS-only path.
All release jobs consume the checked-out source without bumping package files,
committing, or pushing `main`.
Standalone Windows dispatches remain inspection-only.
