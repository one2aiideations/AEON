# mouli-apps

Adapters that turn the application crates into governed capabilities of the main interface
(`mouli-interface`). The chat, the screens and any agent call them the same way, and every call goes
through one gate: policy, approval where the risk calls for it, and the audit chain.

| Application | Capabilities | Needs approval |
|---|---|---|
| Sheet (`mouli-sheet`) | `sheet.list`, `sheet.get`, `sheet.show`, `sheet.set`, `sheet.add`, `sheet.clear` | `sheet.clear` |
| Studio (`mouli-studio`) | `studio.show`, `studio.heading`, `studio.paragraph`, `studio.clear` | `studio.clear` |
| CRM (`mouli-crm`) | `crm.lead.list/capture/qualify/convert`, `crm.opp.list/advance`, `crm.approval.decide`, `crm.forecast` | `crm.approval.decide` |
| Player (`mouli-player`) | `player.status`, `player.queue` (read); `player.add/play/pause/next/prev/stop/seek/volume/mute/speed/shuffle/repeat/autoplay/shorts/rate/jump/remove/clear` | none (all are plain writes; viewers get the two read ones) |
| Code (`mouli-code`) | `code.status`, `code.files`, `code.read`, `code.edit`, `code.propose`, `code.reject`, `code.apply`, `code.rollback` | `code.apply`, `code.rollback` |

`register(&mut workspace)` creates both and returns shared handles so the screens can draw their
state. Screens edit through `Workspace::call` with structured arguments, so a value with quotes or
spaces cannot change which capability runs.

Sheet and Studio data is saved to `sheet.mouli` and `document.mouli` (atomic writes, checked load).
Code works on a project folder that the host chooses, never a call: `code.edit` turns "this file
should now read ..." into a held change, and only `code.apply` (approved by a second person) writes
files; `code.rollback` restores them exactly. One change is held at a time.

Only applications that are light enough to link into the device are here. Project, Mail and
Business pull in web servers and databases, so they need a different route (a separate process
behind the same capabilities).
