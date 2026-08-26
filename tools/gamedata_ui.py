"""Structured editor for the canonical Orrun GameData XML file.

The XML remains an implementation detail: users edit sections, records, and
labeled attributes through the Tkinter form and never see serialized XML.
"""
from __future__ import annotations

import tkinter as tk
from pathlib import Path
from tkinter import filedialog, messagebox, ttk
from xml.etree import ElementTree as ET

from .gamedata import GameData


SECTION_TAGS = ("skills", "factions", "effects", "actions", "players", "mobs", "movement", "hamlet", "defaults")
CHILD_TAGS = {
    "skills": "skill", "factions": "faction", "actions": "action", "players": "profile",
    "mobs": "mob", "movement": "spec", "defaults": "value", "hamlet": "layer",
    "effects": "effect", "mob": "action", "action": "effect", "profile": "skill",
}
DISPLAY_NAMES = {
    "skills": "Skills", "factions": "Factions", "effects": "Effect Catalog", "actions": "Actions", "players": "Player Profiles",
    "mobs": "Mobs", "movement": "Movement", "hamlet": "Hamlet", "defaults": "Defaults",
    "profile": "Player Profile", "spec": "Movement Specification", "value": "Default Value",
    "effect": "Effect", "action": "Action", "mob": "Mob", "skill": "Skill", "faction": "Faction",
}
RECORD_DEFAULTS: dict[str, dict[str, str]] = {
    "mob": {
        "name": "New Mob", "faction": "wild", "mode": "active", "hp": "1",
        "armor": "0", "damage": "1", "movement_id": "walk", "swing_s": "1",
        "reach_m": "1.8", "speed_variance_ratio": "0", "endurance_s": "30",
    },
}


class GameDataEditor:
    def __init__(self, root: tk.Tk, path: Path) -> None:
        self.root = root
        self.path = path
        self.tree = ET.ElementTree()
        self.nodes: dict[str, ET.Element] = {}
        self.fields: dict[str, ttk.Widget] = {}
        self._field_required: set[str] = set()
        self._building = False
        self._build_ui()
        self.load_file()

    def _build_ui(self) -> None:
        self.root.title("Orrun GameData Editor")
        self.root.geometry("1100x720")
        outer = ttk.Frame(self.root, padding=10)
        outer.pack(fill="both", expand=True)

        toolbar = ttk.Frame(outer)
        toolbar.pack(fill="x", pady=(0, 8))
        for label, command in (("Open…", self.open_file), ("Reload", self.load_file), ("Validate", self.validate), ("Save", self.save), ("Save As…", self.save_as)):
            ttk.Button(toolbar, text=label, command=command).pack(side="left", padx=(0, 6))
        ttk.Label(toolbar, text="Edit GameData sections and fields; XML is stored automatically.").pack(side="left", padx=8)

        pane = ttk.PanedWindow(outer, orient="horizontal")
        pane.pack(fill="both", expand=True)
        left = ttk.Frame(pane, padding=(0, 0, 8, 0))
        right = ttk.Frame(pane, padding=(8, 0, 0, 0))
        pane.add(left, weight=1)
        pane.add(right, weight=2)

        self.treeview = ttk.Treeview(left, show="tree", selectmode="browse")
        scroll = ttk.Scrollbar(left, orient="vertical", command=self.treeview.yview)
        self.treeview.configure(yscrollcommand=scroll.set)
        self.treeview.pack(side="left", fill="both", expand=True)
        scroll.pack(side="right", fill="y")
        self.treeview.bind("<<TreeviewSelect>>", self._select)

        self.record_title = ttk.Label(right, text="Select a section or record", font=("Segoe UI", 12, "bold"))
        self.record_title.pack(anchor="w", pady=(0, 8))
        self.form = ttk.Frame(right)
        self.form.pack(fill="both", expand=True)
        self.form.columnconfigure(1, weight=1)
        actions = ttk.Frame(right)
        actions.pack(fill="x", pady=(8, 0))
        ttk.Button(actions, text="Add", command=self.add_child).pack(side="left")
        ttk.Button(actions, text="Delete Selected", command=self.delete_selected).pack(side="left", padx=6)
        ttk.Button(actions, text="Apply Fields", command=self.apply_fields).pack(side="left")
        self.status = ttk.Label(outer, text="")
        self.status.pack(fill="x", pady=(8, 0))
        self.root.protocol("WM_DELETE_WINDOW", self.root.destroy)

    def load_file(self) -> None:
        try:
            self.tree = ET.parse(self.path)
            GameData.from_xml(self.path.read_text(encoding="utf-8"))
        except (OSError, ET.ParseError, ValueError) as exc:
            messagebox.showerror("Unable to load GameData", str(exc))
            self.status.configure(text=f"Load failed: {exc}")
            return
        self._refresh_tree()
        self.status.configure(text=f"Loaded {self.path}")

    def _refresh_tree(self) -> None:
        self._building = True
        self.treeview.delete(*self.treeview.get_children())
        self.nodes.clear()
        root = self.tree.getroot()
        for section in root:
            if section.tag in SECTION_TAGS:
                self._insert_node("", section)
        self._building = False

    def _insert_node(self, parent_id: str, element: ET.Element) -> str:
        label = DISPLAY_NAMES.get(element.tag, element.tag.replace("_", " ").title())
        if element.get("id"):
            label += f" — {element.get('id')}"
        elif element.get("key"):
            label += f" — {element.get('key')}"
        item = self.treeview.insert(parent_id, "end", text=label, open=True)
        self.nodes[item] = element
        for child in element:
            self._insert_node(item, child)
        return item

    def _select(self, _event: object = None) -> None:
        selection = self.treeview.selection()
        if not selection:
            return
        element = self.nodes[selection[0]]
        parent_item = self.treeview.parent(selection[0])
        parent = self.nodes.get(parent_item) if parent_item else None
        self.record_title.configure(text=DISPLAY_NAMES.get(element.tag, element.tag.title()))
        for child in self.form.winfo_children():
            child.destroy()
        self.fields.clear()
        self._field_required.clear()
        for row, (name, value) in enumerate(element.attrib.items()):
            ttk.Label(self.form, text=name.replace("_", " ").title() + ":").grid(row=row, column=0, sticky="w", padx=(0, 10), pady=4)
            choices = self._choices_for(element, name, value, parent)
            immutable = name == "id"
            if choices is None:
                field: ttk.Widget = ttk.Entry(self.form)
                field.insert(0, value)
                if immutable:
                    field.configure(state="readonly")
            else:
                field = ttk.Combobox(self.form, values=choices, state="readonly")
                if value not in choices:
                    # Preserve invalid data visibly, but never silently repair it.
                    field.configure(values=(*choices, value))
                field.set(value)
            field.grid(row=row, column=1, sticky="ew", pady=4)
            self.fields[name] = field
            if immutable or self._is_required(element, name):
                self._field_required.add(name)
        if not element.attrib:
            ttk.Label(self.form, text="This section has no fields. Add records with the Add button.").grid(row=0, column=0, columnspan=2, sticky="w")

    @staticmethod
    def _is_required(element: ET.Element, name: str) -> bool:
        required: dict[str, frozenset[str]] = {
            "skill": frozenset({"name"}),
            "faction": frozenset({"name"}),
            "effect": frozenset({"name", "kind", "skill_id"}),
            "action": frozenset({"name"}),
            "profile": frozenset({"name", "faction"}),
            "mob": frozenset({"name", "faction", "mode", "hp", "damage", "movement_id", "speed_variance_ratio", "endurance_s"}),
            "spec": frozenset({"speed_mps"}),
            "hamlet": frozenset({"enabled", "width", "depth", "kit_catalog"}),
            "value": frozenset({"key", "value"}),
        }
        if element.tag == "effect" and "effect_id" in element.attrib:
            return name in {"effect_id", "magnitude", "application"}
        return name in required.get(element.tag, frozenset())

    def _choices_for(self, element: ET.Element, name: str, current: str, parent: ET.Element | None) -> tuple[str, ...] | None:
        """Return finite choices; None means the value is genuinely free-form."""
        finite: dict[tuple[str, str], tuple[str, ...]] = {
            ("faction", "neutral"): ("true", "false"),
            ("effect", "progression"): ("skill_level", "flat"),
            ("effect", "kind"): ("damage", "heal", "control", "movement", "defense", "utility"),
            ("action", "target"): ("hostile", "friendly", "self", "any", "none"),
            ("mob", "mode"): ("active", "passive"),
            ("hamlet", "enabled"): ("true", "false"),
            ("effect", "application"): ("single_target", "cone", "aoe", "pbaoe"),
        }
        if (element.tag, name) in finite and not (element.tag == "effect" and name == "kind" and "effect_id" in element.attrib):
            return finite[(element.tag, name)]
        if element.tag == "effect" and name == "application" and "effect_id" in element.attrib:
            return ("single_target", "cone", "aoe", "pbaoe")
        reference_sections: dict[tuple[str, str], tuple[str, str]] = {
            ("effect", "skill_id"): ("skills", "skill"),
            ("effect", "effect_id"): ("effects", "effect"),
            ("profile", "faction"): ("factions", "faction"),
            ("mob", "faction"): ("factions", "faction"),
            ("mob", "movement_id"): ("movement", "spec"),
            ("action", "effect_id"): ("effects", "effect"),
        }
        section_info = reference_sections.get((element.tag, name))
        if element.tag == "action" and name == "id" and parent is not None and parent.tag == "mob":
            section_info = ("actions", "action")
        if element.tag == "skill" and name == "id" and parent is not None and parent.tag == "profile":
            section_info = ("skills", "skill")
        if section_info is None:
            return None
        section, child_tag = section_info
        section_node = self.tree.getroot().find(section)
        if section_node is None:
            return ()
        return tuple(node.get("id", "") for node in section_node if node.tag == child_tag and node.get("id"))

    def apply_fields(self) -> None:
        selection = self.treeview.selection()
        if not selection:
            return
        element = self.nodes[selection[0]]
        values = {name: field.get() for name, field in self.fields.items()}
        empty_required = sorted(name for name in self._field_required if not values[name].strip())
        if empty_required:
            messagebox.showerror("Invalid fields", "These fields are required: " + ", ".join(empty_required))
            return
        element.attrib.update(values)
        self._refresh_tree()
        self.status.configure(text="Fields applied; press Save to write GameData")

    def add_child(self) -> None:
        selection = self.treeview.selection()
        if not selection:
            messagebox.showinfo("Add record", "Select a section or parent record first.")
            return
        selected = self.nodes[selection[0]]
        if selected.tag in ("action", "effects"):
            self._add_authored_effect(selected)
            return
        tag = CHILD_TAGS.get(selected.tag)
        if tag is None:
            messagebox.showinfo("Add record", f"No records can be added below {selected.tag}.")
            return
        element = ET.SubElement(selected, tag)
        if tag not in ("layer",):
            existing_ids = {node.get("id") for node in selected}
            candidate = f"new_{tag}"
            suffix = 1
            while candidate in existing_ids:
                suffix += 1
                candidate = f"new_{tag}_{suffix}"
            element.set("id", candidate)
        element.attrib.update(RECORD_DEFAULTS.get(tag, {}))
        self._refresh_tree()
        self.status.configure(text=f"Added {DISPLAY_NAMES.get(tag, tag)}; edit its fields and apply")

    def _add_authored_effect(self, selected: ET.Element) -> None:
        effects_parent = selected if selected.tag == "effects" else selected.find("effects")
        if effects_parent is None:
            effects_parent = ET.SubElement(selected, "effects")
        catalog = self.tree.getroot().find("effects")
        authored = list(catalog.findall("effect")) if catalog is not None else []
        if not authored:
            messagebox.showinfo("Add effect", "No authored effects are available. Add one to the Effect Catalog first.")
            return
        dialog = tk.Toplevel(self.root)
        dialog.title("Choose Effect")
        dialog.transient(self.root)
        dialog.grab_set()
        ttk.Label(dialog, text="Choose an authored effect; edit only its action magnitude afterwards:").pack(anchor="w", padx=12, pady=(12, 6))
        choices = tk.Listbox(dialog, width=82, height=min(12, len(authored)))
        choices.pack(fill="both", expand=True, padx=12)
        for effect in authored:
            choices.insert(tk.END, f"{effect.get('id', '<unnamed>')} — {effect.get('name', effect.get('kind', 'effect'))} / skill: {effect.get('skill_id', '<missing>')} / progression: {effect.get('progression', 'skill_level')}")
        choices.selection_set(0)

        def accept() -> None:
            chosen = authored[choices.curselection()[0]] if choices.curselection() else None
            if chosen is None:
                return
            ET.SubElement(effects_parent, "effect", {"effect_id": chosen.get("id", ""), "magnitude": "1", "application": "single_target", "range_m": "1.8"})
            dialog.destroy()
            self._refresh_tree()
            self.status.configure(text=f"Added {chosen.get('name', chosen.get('id', 'effect'))}; select the assignment to edit magnitude and application geometry")

        buttons = ttk.Frame(dialog)
        buttons.pack(fill="x", padx=12, pady=12)
        ttk.Button(buttons, text="Add Effect", command=accept).pack(side="left")
        ttk.Button(buttons, text="Cancel", command=dialog.destroy).pack(side="left", padx=6)

    def delete_selected(self) -> None:
        selection = self.treeview.selection()
        if not selection:
            return
        item = selection[0]
        element = self.nodes[item]
        parent_item = self.treeview.parent(item)
        if not parent_item:
            messagebox.showwarning("Delete record", "Sections cannot be deleted.")
            return
        parent = self.nodes[parent_item]
        if not messagebox.askyesno("Delete record", f"Delete {DISPLAY_NAMES.get(element.tag, element.tag)}?"):
            return
        parent.remove(element)
        self._refresh_tree()
        self.status.configure(text="Record deleted; press Save to write GameData")

    def validate(self) -> bool:
        try:
            xml = ET.tostring(self.tree.getroot(), encoding="unicode")
            GameData.from_xml(xml)
        except ValueError as exc:
            self.status.configure(text=f"Invalid GameData: {exc}")
            messagebox.showerror("Invalid GameData", str(exc))
            return False
        self.status.configure(text="GameData is valid")
        return True

    def save(self) -> None:
        if not self.validate():
            return
        temporary = self.path.with_suffix(self.path.suffix + ".tmp")
        temporary.write_text(ET.tostring(self.tree.getroot(), encoding="unicode") + "\n", encoding="utf-8")
        temporary.replace(self.path)
        self.status.configure(text=f"Saved {self.path}")

    def open_file(self) -> None:
        selected = filedialog.askopenfilename(filetypes=[("Orrun GameData XML", "*.xml"), ("All files", "*.*")], initialdir=self.path.parent)
        if selected:
            self.path = Path(selected)
            self.load_file()

    def save_as(self) -> None:
        selected = filedialog.asksaveasfilename(defaultextension=".xml", filetypes=[("Orrun GameData XML", "*.xml")], initialfile=self.path.name, initialdir=self.path.parent)
        if selected:
            self.path = Path(selected)
            self.save()


def run(path: Path) -> None:
    root = tk.Tk()
    GameDataEditor(root, path)
    root.mainloop()
