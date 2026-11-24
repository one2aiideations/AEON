# mouli-apps

Adapters that turn the application crates into governed capabilities of the main interface
(`mouli-interface`). The chat, the screens and any agent call them the same way, and every call goes
through one gate: policy, approval where the risk calls for it, and the audit chain.

| Application | Capabilities | Needs approval |
|---|---|---|
| Sheet (`mouli-sheet`) | `sheet.list`, `sheet.get`, `sheet.show`, `sheet.set`, `sheet.add`, `sheet.clear` | `sheet.clear` |
| Studio (`mouli-studio`) | `studio.show`, `studio.heading`, `studio.paragraph`, `studio.clear` | `studio.clear` |

`register(&mut workspace)` creates both and returns shared handles so the screens can draw their
state. Screens edit through `Workspace::call` with structured arguments, so a value with quotes or
spaces cannot change which capability runs.

Only applications that are light enough to link into the device are here. CRM can follow (it needs
just `mouli-auth` and `mouli-interface`). Project, Mail and Business pull in web servers and
databases, so they need a different route (a separate process behind the same capabilities).
