"""Typed spell documents, effect catalogue, validation, and deterministic costs."""
from __future__ import annotations
from dataclasses import dataclass, field
from enum import Enum
import json
from pathlib import Path
from typing import Any, Mapping

SCHEMA_VERSION = 1

class EffectFamily(str, Enum):
    DAMAGE = "damage"; DELIVERY = "delivery"; CONTROL = "control"; RESTORATION = "restoration"; UTILITY = "utility"; MODIFIER = "modifier"
class TargetMode(str, Enum):
    SELF = "self"; FRIENDLY = "friendly"; HOSTILE = "hostile"; AREA = "area"; GROUND = "ground"; ANY = "any"

@dataclass(frozen=True)
class EffectDefinition:
    id: str; family: EffectFamily; base_cost: float; allowed_targets: frozenset[TargetMode]; deliveries: frozenset[str]; defaults: Mapping[str, float] = field(default_factory=dict)

@dataclass(frozen=True)
class SpellEffect:
    effect_id: str
    parameters: Mapping[str, float] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {"id": self.effect_id, "parameters": dict(sorted(self.parameters.items()))}
    @classmethod
    def from_dict(cls, raw: Mapping[str, Any]) -> "SpellEffect":
        if set(raw) - {"id", "parameters"} or not isinstance(raw.get("id"), str): raise ValueError("effect requires only string id and parameters")
        params = raw.get("parameters", {})
        if not isinstance(params, dict) or any(not isinstance(k, str) or isinstance(v, bool) or not isinstance(v, (int, float)) for k,v in params.items()): raise ValueError(f"invalid parameters for effect {raw.get('id')}")
        return cls(raw["id"], {k: float(v) for k,v in params.items()})

@dataclass
class Spell:
    id: str; name: str; description: str = ""; target: TargetMode = TargetMode.HOSTILE; delivery: str = "direct"; effects: list[SpellEffect] = field(default_factory=list); metadata: dict[str, str] = field(default_factory=dict)

    def validate(self, registry: Mapping[str, EffectDefinition]) -> list[str]:
        errors: list[str] = []
        if not self.id or not self.id.replace("_", "").isalnum(): errors.append("id must be non-empty and contain only letters, numbers, and underscores")
        if not self.name.strip(): errors.append("name must not be empty")
        if not self.effects: errors.append("spell must contain at least one effect")
        for index, effect in enumerate(self.effects):
            definition = registry.get(effect.effect_id)
            if definition is None: errors.append(f"effects[{index}]: unknown effect {effect.effect_id!r}"); continue
            if self.target not in definition.allowed_targets and TargetMode.ANY not in definition.allowed_targets: errors.append(f"effects[{index}]: {effect.effect_id} cannot target {self.target.value}")
            if self.delivery not in definition.deliveries: errors.append(f"effects[{index}]: {effect.effect_id} does not support delivery {self.delivery}")
            for key, value in effect.parameters.items():
                if key not in {"magnitude", "duration", "range", "radius", "targets", "cast_time", "cooldown", "persistence"}: errors.append(f"effects[{index}]: unsupported parameter {key!r}")
                if value <= 0: errors.append(f"effects[{index}]: parameter {key} must be positive")
        if self.target == TargetMode.AREA and self.delivery != "area": errors.append("area target requires area delivery")
        if self.target == TargetMode.GROUND and self.delivery != "ground_targeted": errors.append("ground target requires ground_targeted delivery")
        if self.delivery == "chain" and self.target != TargetMode.HOSTILE: errors.append("chain delivery requires hostile targeting")
        return errors

    def cost_breakdown(self, registry: Mapping[str, EffectDefinition]) -> list[tuple[str, float]]:
        errors = self.validate(registry)
        if errors: raise ValueError("cannot price invalid spell: " + "; ".join(errors))
        result: list[tuple[str, float]] = []
        for effect in self.effects:
            definition = registry[effect.effect_id]; cost = definition.base_cost
            for key, value in effect.parameters.items():
                if key == "magnitude": cost *= max(0.25, value)
                elif key == "duration": cost *= 1 + value / 10
                elif key == "radius": cost *= 1 + value / 8
                elif key == "range": cost *= 1 + value / 20
                elif key == "targets": cost *= 1 + max(0, value - 1) / 3
                elif key == "cast_time": cost *= max(0.5, 1 - value / 10)
                elif key == "cooldown": cost *= max(0.5, 1 - value / 30)
                elif key == "persistence": cost *= 1 + value / 10
            result.append((effect.effect_id, round(cost, 4)))
        return result
    def cost(self, registry: Mapping[str, EffectDefinition]) -> float: return round(sum(value for _, value in self.cost_breakdown(registry)), 2)
    def to_dict(self) -> dict[str, Any]: return {"schema_version": SCHEMA_VERSION, "id": self.id, "name": self.name, "description": self.description, "target": self.target.value, "delivery": self.delivery, "effects": [e.to_dict() for e in self.effects], "metadata": dict(sorted(self.metadata.items()))}
    @classmethod
    def from_dict(cls, raw: Mapping[str, Any]) -> "Spell":
        if raw.get("schema_version") != SCHEMA_VERSION: raise ValueError(f"unsupported schema_version: {raw.get('schema_version')!r}")
        allowed = {"schema_version", "id", "name", "description", "target", "delivery", "effects", "metadata"}
        if set(raw) - allowed: raise ValueError("unknown spell fields: " + ", ".join(sorted(set(raw) - allowed)))
        effects = raw.get("effects")
        if not isinstance(effects, list): raise ValueError("effects must be an array")
        return cls(str(raw["id"]), str(raw["name"]), str(raw.get("description", "")), TargetMode(raw["target"]), str(raw["delivery"]), [SpellEffect.from_dict(e) for e in effects], dict(raw.get("metadata", {})))
    def save(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True); temporary = path.with_suffix(path.suffix + ".tmp"); temporary.write_text(json.dumps(self.to_dict(), indent=2) + "\n", encoding="utf-8"); temporary.replace(path)
    @classmethod
    def load(cls, path: Path) -> "Spell": return cls.from_dict(json.loads(path.read_text(encoding="utf-8")))