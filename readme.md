# Files

Files and the system Open / Save As panels share the same browser and sidebar.

## File operations

Select an item and use **Copy / Cut**, navigate to the destination folder, then
**Paste**. The corresponding shortcuts are Ctrl+C, Ctrl+X and Ctrl+V (also Super
on supported keyboards). These shortcuts leave path, search and rename editing
alone. The AppCore clipboard carries typed file references; Linux previews share
its existing clipboard slot between Files processes without exposing paths to
text fields.

The context menu also provides New Folder, Rename and permanently confirmed
Delete. One job runs per Files window. Navigation remains available during a job;
mutating context commands are disabled until it finishes.

The operation area shows processed items and copied bytes, with Cancel / Escape.
Copies are built in a private sibling staging directory and published only when
complete. Cancelling removes the staging copy and preserves the source. A delete
can be stopped between steps; items already deleted cannot be restored. Final
publication and replacement cleanup cannot be cancelled.

Existing destinations require **Replace**, **Keep Both**, **Skip**, or **Cancel**.
Replacing a folder replaces the whole folder rather than merging its contents.
The previous destination is retained until publication succeeds and restored if
publication fails. Self transfers and replacing a source's ancestor are rejected.
A destination that appears during copying triggers the same confirmation.

Jobs execute at most 32 filesystem steps or 3 ms per paint, copying at most 64 KiB
per step, then schedule the next frame. This bounds application work, but an
individual filesystem call can still block on slow storage. Directory iterators
are consumed incrementally rather than loading an entire tree into memory.

Current limits: selection is single-item; moves use filesystem rename (cross-volume
moves report an error); copying symbolic links / special files reports an error
and removes the partial copy. Deleting symbolic links never traverses their
targets. Interrupted processes can leave hidden `.files-transfer-*` recovery
folders; there is no crash recovery service yet.
