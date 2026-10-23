### Connector string methods count characters instead of bytes

`->slice`, `->size`, `->get` and `->has` now count characters rather than bytes on strings, matching `->first` and `->last`. For non-ASCII strings this changes results: `"é"->size` is now `1` (it was `2`), and `"é🙂"->get(1)` is now `"🙂"` (it was an out-of-bounds error).
