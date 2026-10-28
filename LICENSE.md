# Licensing

This repository mixes code from several sources, and **only part of it is under a licence.** This file says exactly which part.

- **Cross-Com Enhanced's own work is under the [MIT licence](#mit-licence)** (full text below).
- **Code from upstream [5th Echelon](https://github.com/unixoide/5th-echelon) is not.** Its authors haven't chosen a licence yet ([unixoide/5th-echelon#129](https://github.com/unixoide/5th-echelon/issues/129)), so it remains theirs, with all rights reserved.
- **Code from others' pull requests to 5th Echelon** stays under its authors' terms.

This project can't license other people's work, and doesn't try to. If upstream adopts a licence, this file will be updated to match.

## What is under the MIT licence

1. **Everything outside `quazal/` and `tools/quazal-tools/`.**
2. **In those two folders, the work of 5th Echelon Enhanced and Cross-Com Enhanced** (both by JDevWebb): the files `quazal/src/rmc/unhandled.rs` and `quazal/src/rmc/failures.rs`, and the lines JDevWebb added or changed in the other files, as recorded in the git history (`git log --author=JDevWebb -- quazal`, `git blame`). The files as a whole are not.

## Not covered by this licence

| What | Whose, and on what terms |
|---|---|
| `quazal/`: the Quazal PRUDP, RMC and Kerberos implementation, apart from the work listed above | unixoide and upstream 5th Echelon contributors; no licence yet |
| `tools/quazal-tools/`: the DDL parser and protocol code generator, apart from the work listed above | unixoide and upstream 5th Echelon contributors; no licence yet |
| Changes to `quazal/` by Matthias Walther, koteykaby and Askorbinovaya Kislota, and to `tools/quazal-tools/` by garruslnun | Their authors |

`quazal/` and `tools/quazal-tools/` came from 5th Echelon Enhanced with their history, so every commit keeps its original author.

**No warranty, for everything here.** Whatever its licence, the software in this repository and its builds are provided "as is", without warranty of any kind, express or implied, including merchantability, fitness for a particular purpose and non-infringement. In no event are the authors liable for any claim, damages or other liability, in contract, tort or otherwise, arising from or in connection with them, their use, or other dealings in them.

Third-party libraries used by the build keep their own licences. They're fetched by Cargo and aren't part of this repository.

Tom Clancy's Ghost Recon: Future Soldier and its names and marks are trademarks of Ubisoft Entertainment. This project isn't affiliated with or endorsed by Ubisoft, and contains no game files.

## Contributions

Unless a pull request says otherwise, contributions to this repository are accepted under the MIT licence.

## MIT licence

Copyright (c) 2026 JDevWebb and Cross-Com Enhanced contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
