# plugins.d — runtime plugin drop-in directory

Compiled plugin libraries (`.so` / `.dll` / `.dylib`) placed here are discovered and
loaded by the engine at startup via the stable-ABI plugin host (`abi_stable`).

Build a plugin, then copy its compiled artifact from `engine/target/<profile>/` into this
directory. The host validates each library's ABI/semver against `plugin-contract` before
registering it; incompatible or unreadable libraries are skipped with a logged warning.

The compiled artifacts themselves are git-ignored — only this README is tracked so the
directory exists in a fresh checkout.
