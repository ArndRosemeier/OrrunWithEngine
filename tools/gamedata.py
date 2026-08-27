"""Strict schema-3 authoring model for canonical Orrun GameData."""
from __future__ import annotations
from dataclasses import dataclass, field
import math
from pathlib import Path
from xml.etree import ElementTree as ET

SCHEMA_VERSION = 3
OPERATIONS = frozenset({"direct_damage", "heal", "root", "hold", "snare", "charm"})
TARGETS = frozenset({"hostile", "friendly", "self", "any", "none"})
APPLICATIONS = frozenset({"single_target", "cone", "aoe", "pbaoe"})
PROGRESSIONS = frozenset({"skill_level", "flat"})
MODES = frozenset({"active", "passive"})
_ID_CHARS = frozenset("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-")

def _id(value: str, label: str) -> str:
    if not value or any(c not in _ID_CHARS for c in value): raise ValueError(f"{label} has invalid id {value!r}")
    return value

def _attrs(n: ET.Element, allowed: set[str], required: set[str] = set()) -> None:
    unknown=set(n.attrib)-allowed; missing=required-set(n.attrib)
    if unknown: raise ValueError(f"{n.tag}: unknown attributes: {', '.join(sorted(unknown))}")
    if missing: raise ValueError(f"{n.tag}: missing attributes: {', '.join(sorted(missing))}")

def _float(n: ET.Element, name: str, default: float | None = None) -> float:
    if name not in n.attrib:
        if default is None: raise ValueError(f"{n.tag}: missing attributes: {name}")
        return default
    try: value=float(n.attrib[name])
    except ValueError as e: raise ValueError(f"{n.tag} @{name} must be numeric") from e
    if not math.isfinite(value): raise ValueError(f"{n.tag} @{name} must be finite")
    return value

def _int(n: ET.Element, name: str, default: int | None = None) -> int:
    if name not in n.attrib:
        if default is None: raise ValueError(f"{n.tag}: missing attributes: {name}")
        return default
    try: return int(n.attrib[name])
    except ValueError as e: raise ValueError(f"{n.tag} @{name} must be an integer") from e

def _bool(n: ET.Element, name: str, default: bool | None = None) -> bool:
    if name not in n.attrib:
        if default is None: raise ValueError(f"{n.tag}: missing attributes: {name}")
        return default
    if n.attrib[name] not in {"true","false"}: raise ValueError(f"{n.tag} @{name} must be true or false")
    return n.attrib[name]=="true"

def _num(v: float) -> str: return f"{v:g}"

def _children(n: ET.Element, tag: str) -> list[ET.Element]:
    unknown={c.tag for c in n}-{tag}
    if unknown: raise ValueError(f"{n.tag}: unknown child elements: {', '.join(sorted(unknown))}")
    return list(n.findall(tag))

@dataclass(frozen=True)
class Skill:
    id:str; name:str; description:str=""; level_scale:float=1.0
    def xml(self,p): ET.SubElement(p,"skill",{"id":self.id,"name":self.name,"description":self.description,"level_scale":_num(self.level_scale)})
@dataclass(frozen=True)
class Faction:
    id:str; name:str; neutral:bool=False
    def xml(self,p): ET.SubElement(p,"faction",{"id":self.id,"name":self.name,"neutral":str(self.neutral).lower()})
@dataclass(frozen=True)
class EffectDefinition:
    id:str; name:str; operation:str; skill_id:str; progression:str="skill_level"
    def xml(self,p): ET.SubElement(p,"effect",{"id":self.id,"name":self.name,"operation":self.operation,"skill_id":self.skill_id,"progression":self.progression})
@dataclass(frozen=True)
class ActionEffect:
    effect_id:str; magnitude:float; application:str; range_m:float; radius_m:float=0.0; angle_deg:float=0.0; duration_s:float=0.0; movement_multiplier:float=1.0
    def xml(self,p):
        a={"effect_id":self.effect_id,"magnitude":_num(self.magnitude),"application":self.application,"range_m":_num(self.range_m),"duration_s":_num(self.duration_s),"movement_multiplier":_num(self.movement_multiplier)}
        if self.radius_m: a["radius_m"]=_num(self.radius_m)
        if self.angle_deg: a["angle_deg"]=_num(self.angle_deg)
        ET.SubElement(p,"effect",a)
@dataclass(frozen=True)
class Action:
    id:str; name:str; target:str; effects:tuple[ActionEffect,...]; description:str=""; mana_cost:float=0.0; cast_s:float=0.0; cooldown_s:float=0.0; interruptible:bool=False; reveals:bool=True
    def xml(self,p):
        a={"id":self.id,"name":self.name,"description":self.description,"target":self.target,"mana_cost":_num(self.mana_cost),"cast_s":_num(self.cast_s),"cooldown_s":_num(self.cooldown_s),"interruptible":str(self.interruptible).lower(),"reveals":str(self.reveals).lower()}
        n=ET.SubElement(p,"action",a); es=ET.SubElement(n,"effects")
        for e in self.effects:e.xml(es)
@dataclass(frozen=True)
class ActorRoster:
    id:str; name:str; faction:str; skills:tuple[tuple[str,int],...]; actions:tuple[str,...]
    def children(self,n):
        for i,l in self.skills: ET.SubElement(n,"skill",{"id":i,"level":str(l)})
        for i in self.actions: ET.SubElement(n,"action",{"id":i})
@dataclass(frozen=True)
class PlayerProfile(ActorRoster):
    def xml(self,p): n=ET.SubElement(p,"profile",{"id":self.id,"name":self.name,"faction":self.faction}); self.children(n)
@dataclass(frozen=True)
class Mob(ActorRoster):
    speed_variance_ratio:float; endurance_s:float; mode:str="active"; hp:int=1; armor:int=0; movement_id:str="walk"; species_id:str=""
    def xml(self,p):
        a={"id":self.id,"name":self.name,"faction":self.faction,"mode":self.mode,"hp":str(self.hp),"armor":str(self.armor),"movement_id":self.movement_id,"speed_variance_ratio":_num(self.speed_variance_ratio),"endurance_s":_num(self.endurance_s)}
        if self.species_id:a["species_id"]=self.species_id
        n=ET.SubElement(p,"mob",a); self.children(n)
@dataclass(frozen=True)
class MovementSpec:
    id:str; speed_mps:float
    def xml(self,p):ET.SubElement(p,"spec",{"id":self.id,"speed_mps":_num(self.speed_mps)})
@dataclass(frozen=True)
class Hamlet:
    enabled:bool=True; width:int=32; depth:int=32; kit_catalog:str="catalogs/medieval.json"; layers:tuple[str,...]=("terrain","roads","dwellings")
    def xml(self,p):
        n=ET.SubElement(p,"hamlet",{"enabled":str(self.enabled).lower(),"width":str(self.width),"depth":str(self.depth),"kit_catalog":self.kit_catalog})
        for i in self.layers:ET.SubElement(n,"layer",{"id":i})

@dataclass
class GameData:
    _skills:tuple[Skill,...]=(); _factions:tuple[Faction,...]=(); _effects:tuple[EffectDefinition,...]=(); _actions:tuple[Action,...]=(); _profiles:tuple[PlayerProfile,...]=(); _mobs:tuple[Mob,...]=(); _movement:tuple[MovementSpec,...]=(); _hamlet:Hamlet=field(default_factory=Hamlet); defaults:dict[str,str]=field(default_factory=dict)
    skills=property(lambda s:s._skills); factions=property(lambda s:s._factions); effects=property(lambda s:s._effects); actions=property(lambda s:s._actions); player_profiles=property(lambda s:s._profiles); mobs=property(lambda s:s._mobs); movement=property(lambda s:s._movement); hamlet=property(lambda s:s._hamlet)
    def validate(self)->list[str]:
        e=[]
        groups=(("skill",self.skills),("faction",self.factions),("effect",self.effects),("action",self.actions),("profile",self.player_profiles),("mob",self.mobs),("movement",self.movement))
        for label,items in groups:
            ids=[x.id for x in items]
            for i in ids:
                try:_id(i,label)
                except ValueError as x:e.append(str(x))
            e += [f"duplicate {label} id {i!r}" for i in sorted({i for i in ids if ids.count(i)>1})]
        skills={x.id for x in self.skills}; factions={x.id for x in self.factions}; effects={x.id:x for x in self.effects}; actions={x.id:x for x in self.actions}; movement={x.id for x in self.movement}
        if sum(x.neutral for x in self.factions)!=1:e.append("exactly one neutral faction is required")
        for x in self.skills:
            if not math.isfinite(x.level_scale):e.append(f"skill {x.id}: level_scale must be finite")
        for x in self.effects:
            if x.operation not in OPERATIONS:e.append(f"effect {x.id}: unknown operation {x.operation!r}")
            if x.skill_id not in skills:e.append(f"effect {x.id}: unknown skill {x.skill_id!r}")
            if x.progression not in PROGRESSIONS:e.append(f"effect {x.id}: unknown progression {x.progression!r}")
        for a in self.actions:
            if a.target not in TARGETS:e.append(f"action {a.id}: unknown target {a.target!r}")
            if not all(math.isfinite(v) for v in (a.mana_cost,a.cast_s,a.cooldown_s)):e.append(f"action {a.id}: timing and mana values must be finite")
            if min(a.mana_cost,a.cast_s,a.cooldown_s)<0:e.append(f"action {a.id}: cast_s, cooldown_s, and mana_cost must be non-negative")
            if not a.effects:e.append(f"action {a.id}: effects must not be empty")
            for q in a.effects:
                vals=(q.magnitude,q.range_m,q.radius_m,q.angle_deg,q.duration_s,q.movement_multiplier)
                if not all(math.isfinite(v) for v in vals):e.append(f"action {a.id}: assignment numbers must be finite");continue
                op=effects.get(q.effect_id)
                if op is None:e.append(f"action {a.id}: unknown effect {q.effect_id!r}");continue
                if q.application not in APPLICATIONS:e.append(f"action {a.id}: unknown application {q.application!r}")
                if q.range_m<0 or q.radius_m<0:e.append(f"action {a.id}: range and radius must be non-negative")
                if q.application in {"single_target","cone","aoe"} and q.range_m<=0:e.append(f"action {a.id}: {q.application} requires positive range")
                if q.application in {"aoe","pbaoe"} and q.radius_m<=0:e.append(f"action {a.id}: {q.application} requires positive radius")
                if q.application=="cone" and not 0<q.angle_deg<=360:e.append(f"action {a.id}: cone angle must be in 0..=360")
                if q.application=="pbaoe" and q.range_m!=0:e.append(f"action {a.id}: pbaoe range must be zero")
                if op.operation in {"direct_damage","heal"} and not(q.magnitude>0 and q.duration_s==0 and q.movement_multiplier==1):e.append(f"action {a.id}: direct operations require positive magnitude, zero duration_s, and movement_multiplier 1")
                if op.operation in {"root","hold","charm"} and not(q.magnitude==1 and q.duration_s>0 and q.movement_multiplier==1):e.append(f"action {a.id}: control operation requires magnitude 1, positive duration_s, and movement_multiplier 1")
                if op.operation=="snare" and not(q.magnitude==1 and q.duration_s>0 and 0<q.movement_multiplier<1):e.append(f"action {a.id}: snare requires magnitude 1, positive duration_s, and movement_multiplier in 0..1")
        for actor in (*self.player_profiles,*self.mobs):
            if actor.faction not in factions:e.append(f"{actor.id}: unknown faction {actor.faction!r}")
            if not actor.actions:e.append(f"{actor.id}: action roster must not be empty")
            known=[]
            for sid,level in actor.skills:
                if sid in known:e.append(f"{actor.id}: duplicate skill {sid!r}")
                known.append(sid)
                if sid not in skills or level<=0:e.append(f"{actor.id}: invalid skill {sid!r} level {level}")
            seen=[]
            for aid in actor.actions:
                if aid in seen:e.append(f"{actor.id}: duplicate action {aid!r}")
                seen.append(aid)
                if aid not in actions:e.append(f"{actor.id}: unknown action {aid!r}");continue
                for q in actions[aid].effects:
                    if q.effect_id in effects and effects[q.effect_id].skill_id not in known:e.append(f"{actor.id}: action {aid!r} requires unassigned skill {effects[q.effect_id].skill_id!r}")
        for m in self.mobs:
            if m.mode not in MODES:e.append(f"mob {m.id}: unknown mode {m.mode!r}")
            if not math.isfinite(m.speed_variance_ratio) or not 0<=m.speed_variance_ratio<=.2:e.append(f"mob {m.id}: speed_variance_ratio must be finite and in 0..=0.20")
            if not math.isfinite(m.endurance_s) or m.endurance_s<=0:e.append(f"mob {m.id}: endurance_s must be finite and positive")
            if m.movement_id not in movement:e.append(f"mob {m.id}: unknown movement {m.movement_id!r}")
            if m.hp<1:e.append(f"mob {m.id}: hp must be positive")
        for x in self.movement:
            if not math.isfinite(x.speed_mps) or x.speed_mps<=0:e.append(f"movement {x.id}: speed_mps must be finite and positive")
        return e
    def to_xml(self)->str:
        errors=self.validate()
        if errors:raise ValueError("invalid GameData: "+"; ".join(errors))
        root=ET.Element("OrrunGameData",{"schema_version":str(SCHEMA_VERSION)})
        for tag,items in (("skills",self.skills),("factions",self.factions),("effects",self.effects),("actions",self.actions),("players",self.player_profiles),("mobs",self.mobs)):
            n=ET.SubElement(root,tag)
            for x in items:x.xml(n)
        n=ET.SubElement(root,"movement")
        for x in self.movement:x.xml(n)
        self.hamlet.xml(root); n=ET.SubElement(root,"defaults")
        for k,v in sorted(self.defaults.items()):ET.SubElement(n,"value",{"key":k,"value":v})
        ET.indent(root,space="  ");return ET.tostring(root,encoding="unicode",short_empty_elements=True)+"\n"
    def save(self,path:Path):path.write_text(self.to_xml(),encoding="utf-8")
    @classmethod
    def load(cls,path:Path):return cls.from_xml(path.read_text(encoding="utf-8"))
    @classmethod
    def from_xml(cls,text:str):
        try:root=ET.fromstring(text)
        except ET.ParseError as x:raise ValueError(f"invalid GameData XML: {x}") from x
        _attrs(root,{"schema_version"},{"schema_version"})
        if root.tag!="OrrunGameData" or root.attrib["schema_version"]!=str(SCHEMA_VERSION):raise ValueError("unsupported or missing GameData schema_version")
        names=("skills","factions","effects","actions","players","mobs","movement","hamlet","defaults")
        if [c.tag for c in root]!=list(names):raise ValueError("GameData sections must appear exactly once in canonical order")
        sections={n:root.find(n) for n in names}
        sk=[]
        for x in _children(sections["skills"],"skill"):_attrs(x,{"id","name","description","level_scale"},{"id","name"});sk.append(Skill(x.attrib["id"],x.attrib["name"],x.get("description",""),_float(x,"level_scale",1)))
        fs=[]
        for x in _children(sections["factions"],"faction"):_attrs(x,{"id","name","neutral"},{"id","name","neutral"});fs.append(Faction(x.attrib["id"],x.attrib["name"],_bool(x,"neutral")))
        es=[]
        for x in _children(sections["effects"],"effect"):_attrs(x,{"id","name","operation","skill_id","progression"},{"id","name","operation","skill_id","progression"});es.append(EffectDefinition(x.attrib["id"],x.attrib["name"],x.attrib["operation"],x.attrib["skill_id"],x.attrib["progression"]))
        actions=[]
        for x in _children(sections["actions"],"action"):
            _attrs(x,{"id","name","description","target","mana_cost","cast_s","cooldown_s","interruptible","reveals"},{"id","name","target","mana_cost","cast_s","cooldown_s","interruptible","reveals"})
            if [c.tag for c in x] != ["effects"]:raise ValueError("action requires exactly one effects child")
            qs=[]
            for q in _children(x.find("effects"),"effect"):
                _attrs(q,{"effect_id","magnitude","application","range_m","radius_m","angle_deg","duration_s","movement_multiplier"},{"effect_id","magnitude","application","range_m","duration_s","movement_multiplier"})
                qs.append(ActionEffect(q.attrib["effect_id"],_float(q,"magnitude"),q.attrib["application"],_float(q,"range_m"),_float(q,"radius_m",0),_float(q,"angle_deg",0),_float(q,"duration_s"),_float(q,"movement_multiplier")))
            actions.append(Action(x.attrib["id"],x.attrib["name"],x.attrib["target"],tuple(qs),x.get("description",""),_float(x,"mana_cost"),_float(x,"cast_s"),_float(x,"cooldown_s"),_bool(x,"interruptible"),_bool(x,"reveals")))
        def roster(x):
            unknown={c.tag for c in x}-{"skill","action"}
            if unknown:raise ValueError(f"{x.tag}: unknown child elements: {unknown}")
            skills=[];actions=[]; seen_action=False
            for c in x:
                if c.tag=="skill":
                    if seen_action:raise ValueError(f"{x.tag}: skills must precede actions")
                    _attrs(c,{"id","level"},{"id","level"});skills.append((c.attrib["id"],_int(c,"level")))
                else:seen_action=True;_attrs(c,{"id"},{"id"});actions.append(c.attrib["id"])
            return tuple(skills),tuple(actions)
        ps=[]
        for x in _children(sections["players"],"profile"):_attrs(x,{"id","name","faction"},{"id","name","faction"});s,a=roster(x);ps.append(PlayerProfile(x.attrib["id"],x.attrib["name"],x.attrib["faction"],s,a))
        ms=[]
        for x in _children(sections["mobs"],"mob"):
            _attrs(x,{"id","name","faction","mode","hp","armor","movement_id","species_id","speed_variance_ratio","endurance_s"},{"id","name","faction","mode","hp","armor","movement_id","speed_variance_ratio","endurance_s"});s,a=roster(x)
            ms.append(Mob(x.attrib["id"],x.attrib["name"],x.attrib["faction"],s,a,_float(x,"speed_variance_ratio"),_float(x,"endurance_s"),x.attrib["mode"],_int(x,"hp"),_int(x,"armor"),x.attrib["movement_id"],x.get("species_id","")))
        movement=[]
        for x in _children(sections["movement"],"spec"):_attrs(x,{"id","speed_mps"},{"id","speed_mps"});movement.append(MovementSpec(x.attrib["id"],_float(x,"speed_mps")))
        h=sections["hamlet"];_attrs(h,{"enabled","width","depth","kit_catalog"},{"enabled","width","depth","kit_catalog"});layers=[]
        for x in _children(h,"layer"):_attrs(x,{"id"},{"id"});layers.append(x.attrib["id"])
        d={}
        for x in _children(sections["defaults"],"value"):_attrs(x,{"key","value"},{"key","value"});d[x.attrib["key"]]=x.attrib["value"]
        out=cls(tuple(sk),tuple(fs),tuple(es),tuple(actions),tuple(ps),tuple(ms),tuple(movement),Hamlet(_bool(h,"enabled"),_int(h,"width"),_int(h,"depth"),h.attrib["kit_catalog"],tuple(layers)),d)
        errors=out.validate()
        if errors:raise ValueError("invalid GameData: "+"; ".join(errors))
        return out
