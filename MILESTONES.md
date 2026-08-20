# Orrun milestones

Living plan. Main only. Invisible live content does not ship. Human controls first. Playtester pixels + Reviewer SHIP close a milestone.

Product locks (not milestones): procgen continent, recipes on existing pins, no authored zones, new seed = new map. Factions Wardens / Cutters / Brood / Tollers. Dungeon-first fill. EQ-soft first-person. Combat bible (trash 5.2, starved in-combat mana, Ember 3/1.2s, potion +40). L1-20 XP curve on paper (~5 hours).

Character assets: dressed humans must include clothing meshes, alphaMode OPAQUE, GLB JSON+BIN\0 only, Idle/Walk present. Engine loader SHIP `ac97df1`: BLEND/MASK without real alpha is a load error (hole-body cannot spawn).

## M0 Playable fight — SHIP

Bars, lock ring, target frame, hit/hurt/swing, death/Shaken, hotbar 1-8+R, log, range-fail.

Live bodies: wolf-spider, orc, tribal, skull orc. Hash `c5f0a7b`.

## M1 Lived-in hamlet — SHIP (2026-08-20)

Chaotic pack stays. Post-pack road cut to the well. Dirt ribbon drawn. Walking humans on clipped glbs, opaque clothes. Hash `9ba80ea`. Playtester PASS + Reviewer SHIP.

## M2 Readable packs - SHIP (2026-08-20)

Three L1 wolves strafe +/-1.8 m. One lock = one body. Wolf Attack still on the locked mesh. play_animation fail-loud. Hash `f2b9a31`. Playtester PASS + Reviewer SHIP.
## M3 Bones in the dark - SHIP (2026-08-20)

T2+ Brood: Warrior + 2 Minion per chamber, Punch_A on the locked mesh. One Mage_Staff pack per dungeon, not on the heart. Staff on the body. Hash `af73aec`. Playtester PASS + Reviewer SHIP.
## M4 Overland logic - NEXT

Packs sit on a site with a visible prop. First stamp: Taken Cairn + Woods Hut (bandits). Then berry theft, ford, silk yard, cut landing, Ash Hold, Sour Well. No tent/campfire mesh yet.

## M5 Path to 20 — QUEUED

Remaining roster from packs we have: Yeti, Demon, BlueDemon, tribal veteran. No fake bows. Hinterland bandit (male_bandit_01, Attack once) after M4.

## M6 Village life 2 — LATER

Fire ring and tents if we get meshes. More civilian loops. Status icons. Kill death pose.

## M7 Ability HUD — LATER

Hotbar CD sweep, cast bars, auto-attack pip, con color when levels mix. Floating combat text and a first-person weapon stay off this board unless we put them on.
