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
## M4 Overland logic - SHIP (2026-08-20)

Packs sit on a visible site. First stamp: Taken Cairn + Woods Hut (L5 bandits). Hut is a small square thatch-hip (straw, half-timber, gray plinth), not a hamlet longhouse. +18 m pin offset dead. Hash `e2f12ab` (hut `fec7850`). Playtester PASS + Reviewer SHIP.

Later stamps (not this close): berry theft, ford, silk yard, cut landing, Ash Hold, Sour Well.

## M5 Path to 20 - SHIP (2026-08-20)

Live roster through L10: Yeti L6 Punch `2ef0de5`, Demon L8 Punch+trident `6476cd8`, BlueDemon L7 Punch `43abc77`, tribal veteran L10 Punch `fc224da`. Playtester PASS + Reviewer on each. Slam/Shout/caster/Pin HOLD (no clips). No fake bows. Hinterland bandit already live on M4 sites.

## M6 Village life 2 - NEXT

Kill death pose SHIP: Engine `6ef3042` play-once holds last frame, Orrun `f275831` (Playtester PASS + Reviewer). Bandit/villagers skip (no Death clip). Player slain path unchanged.

Loot first slice SHIP: I bag, click-corpse loot modal, sparkle on Death pose. No shop, no chest. Playtester PASS sparkle `17ab92a` + modal/bag `bf00fdb`. Reviewer SHIP `17ab92a`. Prior yellow-floor HOLD closed.

Walker/door/alley already live. Status icons belong on M7.

Hamlet yard tent + campfire ring SHIP: cream canvas tent AND stone ring in the T0 hamlet courtyard. Prior dirt FAILS were the wrong hook. Playtester PASS + Reviewer SHIP `camp.png` on `a8a5537` (spawn `7e45c69`).

## M7 Ability HUD — LATER

Hotbar CD radial sweep SHIP: radial pie on Strike, not a curtain. Playtester PASS + Reviewer SHIP `cd_sweep.png` on `947f918`.

Cast bar SHIP: Ember remaining-time bar above the hotbar. Pie left alone. Playtester PASS + Reviewer SHIP `cast_bar.png` on `c8403d7`.

Incoming HP ghost SHIP: red leftover on the 160px bar. Pie/pip/cast bar unchanged. Playtester PASS + Reviewer SHIP `incoming.png` on `9bd8900`.

Con color / status icons / nameplates still later M7. Auto-attack pip. Floating combat text and a first-person weapon stay off this board unless we put them on.
