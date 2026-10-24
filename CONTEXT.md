# Iris Album

Turns the wedding photos (photographer + guests) into print-ready hardcover albums, one per Edition, edited from the browser.

## Language

### Pictures

**Photo**:
One image file on disk, with its own metadata and scores.
_Avoid_: Image, picture, file (when meaning the domain object)

**Shot**:
A group of Photos that show the same picture, whether exact copies, re-compressed forwards, or near-identical frames. Found by perceptual hash or CLIP similarity. Selection, pins and bans apply to Shots.
_Avoid_: Dup group, duplicate set

**Photographer Photo**:
A Photo from the professional photographer. Its folder tells you its Chapter (or a few candidate Chapters), and it serves as the reference for placing Guest Photos.

**Guest Photo**:
A Photo shared by a guest. It has no reliable capture time, so its Chapter is found by looking at it. It's the only source for the groom's preparation.

**Group Portrait**:
A posed Photo of several Persons, taken in front of City Hall after the ceremony. Group Spreads are mostly made of Group Portraits.
_Avoid_: Group photo

**Representative**:
The Photo of a Shot that is used in the album: the highest-resolution, least-compressed copy.
_Avoid_: Master copy, kept copy

### Book structure

**Page**:
One physical side of a leaf, at trim size (297 × 210 mm).

**Spread**:
The two Pages you see together when the book lies open: the unit you edit, lay out and include in Editions. The first and last interior Pages have no facing page; each one is a Spread with one blank side.
_Avoid_: Double page, layout (as a noun for this)

**Chapter**:
One moment of the day, in this fixed order: bride's preparation, groom's preparation, before City Hall, City Hall ceremony, City Hall exit, couple session at the BNF, cocktail, dinner, party. Every Photo and every Spread belongs to exactly one Chapter. A Chapter is introduced by a title on its first Spread, not by a dedicated opener page. The title is always in both French and Tunisian Arabic, at about the same size.
_Avoid_: Section, moment, event

**Template**:
A named arrangement of Slots for a Page or a Spread.
_Avoid_: Layout, grid

**Slot**:
One rectangular area of a Template that holds one Shot or one text block.
_Avoid_: Cell, frame, box

**Crop**:
The part of a Shot's Representative that shows in its Slot.

**Hero**:
A Shot that fills a whole Page or Spread on its own. Used sparingly, because the album favours many photos.
_Avoid_: Full-bleed, highlight

**Cover**:
The front and back of the hardcover. It is not a Spread, and it is the same for every Edition.

### Editions

**Edition**:
One printed version of the album, for one recipient (`ours`, `dad`, `mom`, `bride_parents`). Every Edition is the same classic wedding album, except where it shows its own Group's photos instead of another Group's.
_Avoid_: Version, variant, copy, album (when meaning one Edition)

**Group**:
A named collection of Persons. A Person can belong to several Groups. Each family Edition is tied to one Group, and `ours` is tied to everyone. The Friends Group has no Edition of its own, so its Group Spread is only in `ours`.
_Avoid_: Family, population

**Core Spread**:
A Spread included in every Edition.

**Group Spread**:
A Spread dedicated to one Group's photos, shown only in that Group's Edition and in `ours`.
_Avoid_: Family spread

**Export**:
One numbered, print-ready PDF of one Edition (v1, v2, …), with a record of the album state it was rendered from.
_Avoid_: Render, output, build

### Decisions

**Override**:
A decision the user makes in the GUI. Overrides are kept as a log, survive every regeneration, and can be undone.
_Avoid_: Edit, correction, manual change

**Pin**:
An Override saying a Shot must be in the album, in its Chapter.

**Ban**:
An Override saying a Shot must never be in the album. A Ban beats everything.

**Rejected**:
The computed state of a Shot that fails the quality gate, always with a reason (blurry, too compressed, screenshot, Couple's eyes closed). A Rejected Shot is never selected unless it is Pinned.
_Avoid_: Filtered, failed, banned (Banned is the user's decision)

**Lock**:
A Spread frozen exactly as it is, so regeneration leaves it alone. You can lock a Spread yourself, and any manual layout change (swap, Crop, Template) locks it automatically.

**Proposal**:
The album the generator produces from the Photos and Overrides. Everything that isn't Locked is disposable and can be regenerated.
_Avoid_: Draft, auto-layout

### People

**Person**:
One identified human, made of the faces clustered and labelled as them.
_Avoid_: Face (a face is one detection in one Photo), identity, cluster

**Couple**:
The two Persons with the role Bride and Groom. Most Photos in the album should show them.
_Avoid_: Bride & groom group

**Couple Share**:
The share of Shots in Core Spreads that show at least one member of the Couple (target 70 %). Like a Quota, it's a soft target.

**Quota**:
How many times you'd like a Person to appear in the album. The selector aims for it, but it isn't a hard constraint.
_Avoid_: Weight, boost, minimum
