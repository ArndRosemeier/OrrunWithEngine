from __future__ import annotations
import argparse
from pathlib import Path
from .catalogue import EFFECTS
from .io import validate_file, write_catalog
from .model import Spell
from .templates import starter_templates

def main(argv=None) -> int:
    parser=argparse.ArgumentParser(prog="spell-builder")
    sub=parser.add_subparsers(dest="command",required=True)
    sub.add_parser("list-effects")
    val=sub.add_parser("validate"); val.add_argument("spell",type=Path)
    create=sub.add_parser("create"); create.add_argument("source",type=Path); create.add_argument("output",type=Path)
    temp=sub.add_parser("templates"); temp.add_argument("output",type=Path)
    cat=sub.add_parser("catalog"); cat.add_argument("spells_dir",type=Path); cat.add_argument("output",type=Path)
    sub.add_parser("ui")
    args=parser.parse_args(argv)
    if args.command=="list-effects":
        for id,definition in sorted(EFFECTS.items()): print(f"{id}\t{definition.family.value}\t{definition.base_cost:g}")
    elif args.command=="validate":
        errors=validate_file(args.spell)
        if errors:
            print("INVALID"); print("\n".join(f"- {error}" for error in errors)); return 1
        spell=Spell.load(args.spell); print(f"VALID {spell.id} cost={spell.cost(EFFECTS):g}")
    elif args.command=="create":
        spell=Spell.load(args.source); errors=spell.validate(EFFECTS)
        if errors: print("\n".join(errors)); return 1
        spell.save(args.output); print(args.output)
    elif args.command=="templates":
        args.output.mkdir(parents=True,exist_ok=True)
        for spell in starter_templates().values(): spell.save(args.output/(spell.id+".json"))
        print(f"wrote {len(starter_templates())} templates to {args.output}")
    elif args.command=="catalog": write_catalog(args.spells_dir,args.output); print(args.output)
    elif args.command=="ui":
        from .ui import run; run()
    return 0
if __name__ == "__main__": raise SystemExit(main())