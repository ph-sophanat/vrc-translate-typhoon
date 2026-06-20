# PyInstaller spec for the Typhoon ASR service (server.py).
#
# Produces a standalone onedir build at dist/server/ that needs NO Python on the
# target machine. NeMo + torch do a lot of dynamic importing and ship YAML/config
# data files, so we collect_all the trouble packages rather than relying on the
# auto-discovery, which misses them.
#
# Build:  .venv\Scripts\pyinstaller build_server.spec --noconfirm
#
# onedir (not onefile): torch is ~4 GB; onefile would unpack that to a temp dir on
# every launch. onedir keeps it on disk next to server.exe.

from PyInstaller.utils.hooks import collect_all, collect_submodules

datas, binaries, hiddenimports = [], [], []

# Packages that need *everything* (python modules + data files + dylibs) pulled in.
_collect = [
    "nemo",
    "hydra",
    "omegaconf",
    "antlr4",          # omegaconf grammar
    "sentencepiece",
    "librosa",
    "soundfile",
    "numba",
    "llvmlite",
    "transformers",
    "huggingface_hub",
    "einops",
    "braceexpand",
    "webdataset",
    "sacremoses",
    "lightning",
    "pytorch_lightning",
    "lightning_fabric",
    "lightning_utilities",
    "ruamel",
    "ruamel.yaml",
    "torchmetrics",
    "text_unidecode",
    "editdistance",
    "pyannote",
    # NeMo's cuda_python_utils hard-imports cuda.bindings.driver even on CPU.
    # These are compiled .pyd modules reached via Cython cimport, so PyInstaller's
    # scanner never sees them — collect the whole package explicitly.
    "cuda",
]
for pkg in _collect:
    try:
        d, b, h = collect_all(pkg)
        datas += d
        binaries += b
        hiddenimports += h
    except Exception as e:
        print(f"[spec] collect_all({pkg!r}) skipped: {e}")

# torch: rely on the official PyInstaller hook for the heavy lifting, but make
# sure all submodules are visible (NeMo imports torch.* lazily).
hiddenimports += collect_submodules("torch")
hiddenimports += collect_submodules("torchaudio") if False else []

# A few stragglers NeMo touches by string.
hiddenimports += [
    "scipy.special.cython_special",
    "sklearn.utils._typedefs",
    "sklearn.neighbors._partition_nodes",
    "pkg_resources.extern",
    "cuda.bindings.driver",
    "cuda.bindings.cydriver",
    "cuda.bindings._bindings.cydriver",
    "cuda.bindings.runtime",
    "cuda.bindings.cyruntime",
]


a = Analysis(
    ["server.py"],
    pathex=[],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=["tkinter"],
    noarchive=False,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="server",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=True,
    disable_windowed_traceback=False,
)

coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="server",
)
