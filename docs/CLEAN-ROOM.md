# How protocol knowledge enters the code

nvrview implements an undocumented, proprietary protocol. To keep the code
cleanly ours and the project safe to license as MIT OR Apache-2.0, protocol
knowledge enters the code only as **facts**, written up first, then
implemented from the write-up.

## Sources and what may be taken from each

| Source | May use | Never |
| --- | --- | --- |
| Traffic between our own devices and our own clients | facts observed (command numbers, field names, layouts, sequences) | the captured bytes themselves, which hold credentials, UIDs and addresses (see SANITIZATION.md) |
| The official Reolink apps and SDK (static or runtime analysis) | facts about behavior and wire format | decompiled or disassembled code, symbol dumps, or anything pasted from them |
| Open-source projects such as neolink (AGPL-3.0) or reolink_aio | facts as hints, to be confirmed on hardware | copying or closely paraphrasing code, comments, file structure, or naming |
| Private prior work by the maintainer | facts and approach | names, comments, examples, or anything describing a specific installation |

Copyright covers code as written, not facts about a protocol. Writing from our
own notes, rather than from someone else's source open beside the editor, is
what keeps that line clear.

## The workflow

1. Learn a fact from one of the sources above.
2. Record it in `docs/protocol.md` with its provenance:
   - **observed**: confirmed on real hardware (state the device class and
     firmware family, never a serial, UID, or address)
   - **reported**: from another project's notes or code, not yet confirmed
3. Implement from `docs/protocol.md`, not from the source.
4. Add a synthetic test fixture that exercises it.
5. Upgrade **reported** facts to **observed** before a release depends on them.

## Device safety

The viewer never changes device configuration. The session layer refuses any
command number not on its allowlist; widening the allowlist needs a reason
recorded in `docs/protocol.md`. Camera motion control (preset recall, moves,
focus, a doorbell's speaker) is a separate tier that is off unless the user
enables it per camera (docs/ARCHITECTURE.md, "Device commands"). Device sessions are scarce: one per device, always closed on
every exit path.
