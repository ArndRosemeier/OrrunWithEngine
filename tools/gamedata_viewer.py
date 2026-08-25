"""Launch the GameData viewer/editor."""
from __future__ import annotations
import argparse
from pathlib import Path
from .gamedata import GameData

def main(argv: list[str] | None = None) -> int:
    parser=argparse.ArgumentParser(prog="gamedata-viewer"); parser.add_argument("path",nargs="?",type=Path,default=Path("data/OrrunGameData.xml")); parser.add_argument("--validate",action="store_true")
    args=parser.parse_args(argv); data=GameData.load(args.path)
    if args.validate: print("VALID",args.path); return 0
    from .gamedata_ui import run; run(args.path); return 0
if __name__ == "__main__": raise SystemExit(main())
