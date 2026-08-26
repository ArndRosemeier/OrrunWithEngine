"""Typed, strict, canonical XML authoring model for Orrun game data."""
from __future__ import annotations
from dataclasses import dataclass, field
import math
from pathlib import Path
from typing import Iterable
from xml.etree import ElementTree as ET

SCHEMA_VERSION = 2
_ID_CHARS = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-")

def _id(value: str, label: str) -> str:
    if not value or any(c not in _ID_CHARS for c in value): raise ValueError(f"{label} must be non-empty and contain only letters, numbers, underscores, or hyphens")
    return value

def _text(node: ET.Element, name: str, default: str = "") -> str:
    return node.get(name, default)

def _float(node: ET.Element, name: str, default: float) -> float:
    try:
        value = float(node.get(name, str(default)))
    except ValueError as exc:
        raise ValueError(f"{node.tag} @{name} must be numeric") from exc
    if not math.isfinite(value):
        raise ValueError(f"{node.tag} @{name} must be finite")
    return value


def _required_float(node: ET.Element, name: str) -> float:
    if name not in node.attrib:
        raise ValueError(f"{node.tag}: missing attributes: {name}")
    return _float(node, name, 0.0)

def _int(node: ET.Element, name: str, default: int) -> int:
    try: return int(node.get(name, str(default)))
    except ValueError as exc: raise ValueError(f"{node.tag} @{name} must be an integer") from exc

def _children(node: ET.Element, tag: str) -> list[ET.Element]: return list(node.findall(tag))

def _require_attrs(node: ET.Element, allowed: set[str], required: set[str]) -> None:
    unknown = set(node.attrib) - allowed
    if unknown: raise ValueError(f"{node.tag}: unknown attributes: {', '.join(sorted(unknown))}")
    missing = required - set(node.attrib)
    if missing: raise ValueError(f"{node.tag}: missing attributes: {', '.join(sorted(missing))}")

@dataclass(frozen=True)
class Skill:
    id: str; name: str; description: str = ""; level_scale: float = 1.0
    def __post_init__(self): _id(self.id, "skill id");
    def xml(self, parent: ET.Element) -> None: ET.SubElement(parent, "skill", {"id": self.id, "name": self.name, "description": self.description, "level_scale": _num(self.level_scale)})

@dataclass(frozen=True)
class Faction:
    id: str; name: str; neutral: bool = False
    def xml(self, parent: ET.Element) -> None: ET.SubElement(parent, "faction", {"id": self.id, "name": self.name, "neutral": str(self.neutral).lower()})

@dataclass(frozen=True)
class EffectDefinition:
    id: str; name: str; kind: str; skill_id: str; progression: str = "skill_level"
    def xml(self, parent: ET.Element) -> None:
        ET.SubElement(parent, "effect", {"id": self.id, "name": self.name, "kind": self.kind, "skill_id": self.skill_id, "progression": self.progression})

@dataclass(frozen=True)
class ActionEffect:
    effect_id: str; magnitude: float = 1.0; application: str = "single_target"; range_m: float = 1.8; radius_m: float = 0.0; angle_deg: float = 0.0
    def xml(self, parent: ET.Element) -> None:
        attrs = {"effect_id": self.effect_id, "magnitude": _num(self.magnitude), "application": self.application, "range_m": _num(self.range_m)}
        if self.application in {"aoe", "pbaoe"}: attrs["radius_m"] = _num(self.radius_m)
        if self.application == "cone": attrs["angle_deg"] = _num(self.angle_deg)
        ET.SubElement(parent, "effect", attrs)

@dataclass(frozen=True)
class Action:
    id: str; name: str; target: str = "hostile"; effects: tuple[ActionEffect, ...] = (); description: str = ""; mana_cost: float = 0.0; cast_s: float = 0.0; cooldown_s: float = 0.0
    def xml(self, parent: ET.Element) -> None:
        attrs = {"id": self.id, "name": self.name, "target": self.target, "description": self.description}
        if self.mana_cost: attrs["mana_cost"] = _num(self.mana_cost)
        if self.cast_s: attrs["cast_s"] = _num(self.cast_s)
        if self.cooldown_s: attrs["cooldown_s"] = _num(self.cooldown_s)
        node = ET.SubElement(parent, "action", attrs)
        effects = ET.SubElement(node, "effects")
        for effect in self.effects: effect.xml(effects)

@dataclass(frozen=True)
class PlayerProfile:
    id: str; name: str; faction: str = "citizen"; skills: tuple[tuple[str, int], ...] = ()
    def xml(self, parent: ET.Element) -> None:
        node = ET.SubElement(parent, "profile", {"id": self.id, "name": self.name, "faction": self.faction})
        for skill, level in self.skills: ET.SubElement(node, "skill", {"id": skill, "level": str(level)})

@dataclass(frozen=True)
class Mob:
    id: str; name: str; speed_variance_ratio: float; endurance_s: float; faction: str = "wild"; mode: str = "active"; hp: int = 1; armor: int = 0; damage: int = 1; movement_id: str = "walk"; species_id: str = ""; swing_s: float = 1.0; reach_m: float = 1.8; actions: tuple[str, ...] = ()
    def xml(self, parent: ET.Element) -> None:
        attrs = {"id": self.id, "name": self.name, "faction": self.faction, "mode": self.mode, "hp": str(self.hp), "armor": str(self.armor), "damage": str(self.damage), "movement_id": self.movement_id, "swing_s": _num(self.swing_s), "reach_m": _num(self.reach_m), "speed_variance_ratio": _num(self.speed_variance_ratio), "endurance_s": _num(self.endurance_s)}
        if self.species_id: attrs["species_id"] = self.species_id
        node = ET.SubElement(parent, "mob", attrs)
        for action in self.actions: ET.SubElement(node, "action", {"id": action})

@dataclass(frozen=True)
class MovementSpec:
    id: str; speed_mps: float
    def xml(self, parent: ET.Element) -> None: ET.SubElement(parent, "spec", {"id": self.id, "speed_mps": _num(self.speed_mps)})

@dataclass(frozen=True)
class Hamlet:
    enabled: bool = True; width: int = 32; depth: int = 32; kit_catalog: str = "catalogs/medieval.json"; layers: tuple[str, ...] = ("terrain", "roads", "dwellings")
    def xml(self, parent: ET.Element) -> None:
        node = ET.SubElement(parent, "hamlet", {"enabled": str(self.enabled).lower(), "width": str(self.width), "depth": str(self.depth), "kit_catalog": self.kit_catalog})
        for layer in self.layers: ET.SubElement(node, "layer", {"id": layer})

def _num(value: float) -> str: return f"{value:g}"

@dataclass
class GameData:
    _skills: tuple[Skill, ...] = (); _factions: tuple[Faction, ...] = (); _effects: tuple[EffectDefinition, ...] = (); _actions: tuple[Action, ...] = (); _profiles: tuple[PlayerProfile, ...] = (); _mobs: tuple[Mob, ...] = (); _movement: tuple[MovementSpec, ...] = (); _hamlet: Hamlet = field(default_factory=Hamlet); defaults: dict[str, str] = field(default_factory=dict)
    @property
    def skills(self) -> tuple[Skill, ...]: return self._skills
    @property
    def factions(self) -> tuple[Faction, ...]: return self._factions
    @property
    def effects(self) -> tuple[EffectDefinition, ...]: return self._effects
    @property
    def actions(self) -> tuple[Action, ...]: return self._actions
    @property
    def player_profiles(self) -> tuple[PlayerProfile, ...]: return self._profiles
    @property
    def mobs(self) -> tuple[Mob, ...]: return self._mobs
    @property
    def movement(self) -> tuple[MovementSpec, ...]: return self._movement
    @property
    def hamlet(self) -> Hamlet: return self._hamlet
    def validate(self) -> list[str]:
        errors: list[str] = []
        for label, values in (("skill", self.skills), ("faction", self.factions), ("effect", self.effects), ("action", self.actions), ("profile", self.player_profiles), ("mob", self.mobs), ("movement", self.movement)):
            ids = [x.id for x in values]
            errors.extend(f"duplicate {label} id {item!r}" for item in sorted({x for x in ids if ids.count(x) > 1}))
        factions = {x.id for x in self.factions}; skills = {x.id for x in self.skills}; effects = {x.id for x in self.effects}; actions = {x.id for x in self.actions}; movement = {x.id for x in self.movement}
        if sum(f.neutral for f in self.factions) != 1: errors.append("exactly one neutral faction is required")
        for effect in self.effects:
            if effect.kind not in {"damage", "heal", "control", "movement", "defense", "utility"}: errors.append(f"effect {effect.id}: unknown kind {effect.kind!r}")
            if effect.skill_id not in skills: errors.append(f"effect {effect.id}: unknown skill {effect.skill_id!r}")
            if effect.progression not in {"skill_level", "flat"}: errors.append(f"effect {effect.id}: unknown progression {effect.progression!r}")
        for action in self.actions:
            for assignment in action.effects:
                if assignment.effect_id not in effects: errors.append(f"action {action.id}: unknown effect {assignment.effect_id!r}")
                if assignment.magnitude <= 0: errors.append(f"action {action.id}: effect magnitude must be positive")
                if assignment.application not in {"single_target", "cone", "aoe", "pbaoe"}: errors.append(f"action {action.id}: unknown application {assignment.application!r}")
                if assignment.range_m < 0 or assignment.radius_m < 0: errors.append(f"action {action.id}: range and radius must be non-negative")
                if assignment.application in {"single_target", "cone", "aoe"} and assignment.range_m <= 0: errors.append(f"action {action.id}: {assignment.application} requires positive range")
                if assignment.application in {"aoe", "pbaoe"} and assignment.radius_m <= 0: errors.append(f"action {action.id}: {assignment.application} requires positive radius")
                if assignment.application == "cone" and not 0 < assignment.angle_deg <= 360: errors.append(f"action {action.id}: cone angle must be between 0 and 360 degrees")
                if assignment.application == "pbaoe" and assignment.range_m != 0: errors.append(f"action {action.id}: pbaoe range must be zero")
        for effect in self.effects:
            if effect.skill_id not in skills: errors.append(f"effect {effect.id}: unknown skill {effect.skill_id!r}")
            if effect.progression not in {"skill_level", "flat"}: errors.append(f"effect {effect.id}: unknown progression {effect.progression!r}")
        for actor in (*self.player_profiles, *self.mobs):
            if actor.faction not in factions: errors.append(f"{actor.id}: unknown faction {actor.faction!r}")
        for profile in self.player_profiles:
            for known in profile.skills:
                if known[0] not in skills: errors.append(f"profile {profile.id}: unknown skill {known[0]!r}")
                if known[1] <= 0: errors.append(f"profile {profile.id}: skill {known[0]!r} level must be positive")
        for action in self.actions:
            if action.target not in {"hostile", "friendly", "self", "any", "none"}: errors.append(f"action {action.id}: unknown target {action.target!r}")
            if action.mana_cost < 0: errors.append(f"action {action.id}: mana_cost must be non-negative")
            if action.cast_s < 0 or action.cooldown_s < 0: errors.append(f"action {action.id}: cast_s and cooldown_s must be non-negative")
        for mob in self.mobs:
            if not math.isfinite(mob.speed_variance_ratio) or not 0.0 <= mob.speed_variance_ratio <= 0.20: errors.append(f"mob {mob.id}: speed_variance_ratio must be finite and in 0..=0.20")
            if not math.isfinite(mob.endurance_s) or mob.endurance_s <= 0.0: errors.append(f"mob {mob.id}: endurance_s must be finite and positive")
            if mob.mode not in {"active", "passive"}: errors.append(f"mob {mob.id}: unknown mode {mob.mode!r}")
        for mob in self.mobs:
            for action in mob.actions:
                if action not in actions: errors.append(f"mob {mob.id}: unknown action {action!r}")
            if mob.movement_id not in movement: errors.append(f"mob {mob.id}: unknown movement {mob.movement_id!r}")
            if mob.hp < 1 or mob.damage < 1 or mob.swing_s <= 0 or mob.reach_m <= 0: errors.append(f"mob {mob.id}: combat values must be positive")
        for spec in self.movement:
            if spec.speed_mps <= 0: errors.append(f"movement {spec.id}: speed_mps must be positive")
        return errors
    def to_xml(self) -> str:
        errors = self.validate()
        if errors: raise ValueError("invalid GameData: " + "; ".join(errors))
        root = ET.Element("OrrunGameData", {"schema_version": str(SCHEMA_VERSION)})
        sections = [("skills", self.skills), ("factions", self.factions), ("effects", self.effects), ("actions", self.actions), ("players", self.player_profiles), ("mobs", self.mobs)]
        for tag, values in sections:
            node = ET.SubElement(root, tag)
            for value in values: value.xml(node)
        movement = ET.SubElement(root, "movement")
        for value in self.movement: value.xml(movement)
        self._hamlet.xml(root)
        defaults = ET.SubElement(root, "defaults")
        for key, value in sorted(self.defaults.items()): ET.SubElement(defaults, "value", {"key": key, "value": value})
        ET.indent(root, space="  ")
        return ET.tostring(root, encoding="unicode", short_empty_elements=True) + "\n"
    def save(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True); temporary = path.with_suffix(path.suffix + ".tmp"); temporary.write_text(self.to_xml(), encoding="utf-8"); temporary.replace(path)
    @classmethod
    def load(cls, path: Path) -> "GameData": return cls.from_xml(path.read_text(encoding="utf-8"))
    @classmethod
    def from_xml(cls, text: str) -> "GameData":
        try: root = ET.fromstring(text)
        except ET.ParseError as exc: raise ValueError(f"invalid GameData XML: {exc}") from exc
        if root.tag != "OrrunGameData" or root.get("schema_version") != str(SCHEMA_VERSION): raise ValueError("unsupported or missing GameData schema_version")
        allowed = {"schema_version"}; _require_attrs(root, allowed, {"schema_version"})
        expected_sections = {"skills", "factions", "effects", "actions", "players", "mobs", "movement", "hamlet", "defaults"}
        unknown_sections = {child.tag for child in root} - expected_sections
        if unknown_sections: raise ValueError("unknown GameData sections: " + ", ".join(sorted(unknown_sections)))
        skills_node = root.find("skills"); factions_node = root.find("factions"); effects_node = root.find("effects"); actions_node = root.find("actions"); players_node = root.find("players"); mobs_node = root.find("mobs"); movement_node = root.find("movement"); hamlet_node = root.find("hamlet"); defaults_node = root.find("defaults")
        if any(x is None for x in (skills_node, factions_node, effects_node, actions_node, players_node, mobs_node, movement_node, hamlet_node, defaults_node)): raise ValueError("GameData requires skills, factions, actions, players, mobs, hamlet, and defaults sections")
        for section, child_tag in ((skills_node, "skill"), (factions_node, "faction"), (effects_node, "effect"), (actions_node, "action"), (players_node, "profile"), (mobs_node, "mob")):
            unknown = {child.tag for child in section} - {child_tag}
            if unknown: raise ValueError(f"{section.tag}: unknown child elements: {", ".join(sorted(unknown))}")
        skills = tuple(Skill(_text(x,"id"), _text(x,"name"), _text(x,"description"), _float(x,"level_scale",1.0)) for x in _children(skills_node,"skill"))
        factions = tuple(Faction(_text(x,"id"), _text(x,"name"), _text(x,"neutral","false")=="true") for x in _children(factions_node,"faction"))
        effects = tuple(EffectDefinition(_text(x,"id"), _text(x,"name"), _text(x,"kind"), _text(x,"skill_id"), _text(x,"progression","skill_level")) for x in _children(effects_node,"effect"))
        actions=[]
        for x in _children(actions_node,"action"):
            e_node=x.find("effects"); actions.append(Action(_text(x,"id"),_text(x,"name"),_text(x,"target","hostile"),tuple(ActionEffect(_text(e,"effect_id"), _float(e,"magnitude",1.0), _text(e,"application","single_target"), _float(e,"range_m",1.8), _float(e,"radius_m",0.0), _float(e,"angle_deg",0.0)) for e in _children(e_node,"effect")),_text(x,"description"),_float(x,"mana_cost",0.0),_float(x,"cast_s",0.0),_float(x,"cooldown_s",0.0)))
        profiles=[]
        for x in _children(players_node,"profile"): profiles.append(PlayerProfile(_text(x,"id"),_text(x,"name"),_text(x,"faction","citizen"),tuple((_text(s,"id"),_int(s,"level",1)) for s in _children(x,"skill"))))
        mobs=[]
        for x in _children(mobs_node,"mob"):
            _require_attrs(x, {"id", "name", "faction", "mode", "hp", "armor", "damage", "movement_id", "species_id", "swing_s", "reach_m", "speed_variance_ratio", "endurance_s"}, {"id", "name", "faction", "mode", "hp", "damage", "movement_id", "speed_variance_ratio", "endurance_s"})
            mobs.append(Mob(id=_text(x,"id"), name=_text(x,"name"), speed_variance_ratio=_required_float(x,"speed_variance_ratio"), endurance_s=_required_float(x,"endurance_s"), faction=_text(x,"faction"), mode=_text(x,"mode"), hp=_int(x,"hp",1), armor=_int(x,"armor",0), damage=_int(x,"damage",1), movement_id=_text(x,"movement_id"), species_id=_text(x,"species_id"), swing_s=_float(x,"swing_s",1.0), reach_m=_float(x,"reach_m",1.8), actions=tuple(_text(a,"id") for a in _children(x,"action"))))
        movement=tuple(MovementSpec(_text(x,"id"),_float(x,"speed_mps",1.0)) for x in _children(movement_node,"spec"))
        hamlet=Hamlet(_text(hamlet_node,"enabled","true")=="true",_int(hamlet_node,"width",32),_int(hamlet_node,"depth",32),_text(hamlet_node,"kit_catalog","catalogs/medieval.json"),tuple(_text(x,"id") for x in _children(hamlet_node,"layer")))
        defaults={_text(x,"key"):_text(x,"value") for x in _children(defaults_node,"value")}
        result=cls(skills,factions,effects,tuple(actions),tuple(profiles),tuple(mobs),movement,hamlet,defaults); errors=result.validate()
        if errors: raise ValueError("invalid GameData: " + "; ".join(errors))
        return result
