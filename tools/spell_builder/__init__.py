"""Standalone, data-driven spell authoring package."""
from .model import Spell, SpellEffect
from .catalogue import EFFECTS

__all__ = ["Spell", "SpellEffect", "EFFECTS"]