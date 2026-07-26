# Third-party license records

Licenses stay close to the code they govern:

- the repository's project-owned code is covered by the root
  [`LICENSE-MIT`](../LICENSE-MIT) and
  [`LICENSE-APACHE`](../LICENSE-APACHE);
- vendored projects that include their own license files retain those files
  beside their source, such as
  [`vendor/ch32-hal/LICENSE-MIT`](../vendor/ch32-hal/LICENSE-MIT) and
  [`vendor/ch32-hal/LICENSE-APACHE`](../vendor/ch32-hal/LICENSE-APACHE);
- this directory contains inherited license texts for retained upstream code
  whose vendored subtree did not include a standalone license file.

See [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) for provenance,
upstream revisions, modifications, and the license that applies to each
vendored component. Centralizing every license here would separate vendored
source from its governing notice and duplicate the root project license.
