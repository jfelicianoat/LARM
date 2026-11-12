# llama.cpp runtime adapter work

The first P060 increment adds a read-only Windows discovery and probe helper in
[`probe.py`](probe.py). It examines only roots supplied by the operator, plus
their direct side-by-side build folders. It records hashes for the runtime
executables and conversion scripts, exact version/build output hashes, the
advertised help options, and backend names printed by `--list-devices`.

The probe does not install or start llama.cpp and does not create a profile
lock. It records versions as `custom` until a version-specific profile and
qualification evidence exist. It does not infer CPU support or a GPU backend
from a build label.

This is a partial P060 implementation. No runtime Adapter Protocol package,
managed installation plan, or live llama.cpp observation is present yet. Those
remain blocked until the dependencies and two real Windows builds required by
P060 are available.
