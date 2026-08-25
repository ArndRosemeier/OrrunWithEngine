"""Small Tkinter editor; imports tkinter only when the UI is launched."""
from __future__ import annotations
import tkinter as tk
from tkinter import messagebox, ttk
from .catalogue import EFFECTS
from .model import Spell, SpellEffect, TargetMode
from .templates import starter_templates

class SpellEditor:
    def __init__(self, root: tk.Tk) -> None:
        self.root=root; self.effects: list[SpellEffect]=[]; root.title("Orrun Spell Builder")
        frame=ttk.Frame(root,padding=12); frame.grid(sticky="nsew"); root.columnconfigure(0,weight=1); root.rowconfigure(0,weight=1)
        ttk.Label(frame,text="Spell name").grid(row=0,column=0,sticky="w"); self.name=ttk.Entry(frame,width=40); self.name.grid(row=0,column=1,sticky="ew")
        ttk.Label(frame,text="ID").grid(row=1,column=0,sticky="w"); self.ident=ttk.Entry(frame,width=40); self.ident.grid(row=1,column=1,sticky="ew")
        ttk.Label(frame,text="Target").grid(row=2,column=0,sticky="w"); self.target=ttk.Combobox(frame,values=[x.value for x in TargetMode],state="readonly"); self.target.set("hostile"); self.target.grid(row=2,column=1,sticky="ew")
        ttk.Label(frame,text="Delivery").grid(row=3,column=0,sticky="w"); self.delivery=ttk.Combobox(frame,values=["direct","projectile","cone","area","chain","ground_targeted"],state="readonly"); self.delivery.set("direct"); self.delivery.grid(row=3,column=1,sticky="ew")
        ttk.Label(frame,text="Effect").grid(row=4,column=0,sticky="w"); self.effect=ttk.Combobox(frame,values=sorted(EFFECTS),state="readonly"); self.effect.grid(row=4,column=1,sticky="ew")
        ttk.Button(frame,text="Add effect",command=self.add).grid(row=4,column=2)
        self.listbox=tk.Listbox(frame,height=8,width=55); self.listbox.grid(row=5,column=0,columnspan=3,sticky="nsew",pady=8)
        self.summary=ttk.Label(frame,text="Select effects and validate"); self.summary.grid(row=6,column=0,columnspan=3,sticky="w")
        ttk.Button(frame,text="Load template",command=self.load_template).grid(row=7,column=0); ttk.Button(frame,text="Validate",command=self.validate).grid(row=7,column=1); ttk.Button(frame,text="Save JSON…",command=self.save).grid(row=7,column=2)
        frame.columnconfigure(1,weight=1); frame.rowconfigure(5,weight=1)
    def add(self) -> None:
        value=self.effect.get()
        if value: self.effects.append(SpellEffect(value)); self.listbox.insert(tk.END,value)
    def current(self) -> Spell:
        return Spell(self.ident.get(),self.name.get(),target=TargetMode(self.target.get()),delivery=self.delivery.get(),effects=list(self.effects))
    def validate(self) -> None:
        spell=self.current(); errors=spell.validate(EFFECTS)
        self.summary.configure(text=("Valid — cost %.2f" % spell.cost(EFFECTS)) if not errors else "Invalid: " + "; ".join(errors))
    def load_template(self) -> None:
        spell=starter_templates()["fire_bolt"]; self.ident.delete(0,tk.END); self.ident.insert(0,spell.id); self.name.delete(0,tk.END); self.name.insert(0,spell.name); self.effects=list(spell.effects); self.listbox.delete(0,tk.END); [self.listbox.insert(tk.END,e.effect_id) for e in self.effects]; self.target.set(spell.target.value); self.delivery.set(spell.delivery)
    def save(self) -> None:
        from tkinter.filedialog import asksaveasfilename
        spell=self.current(); errors=spell.validate(EFFECTS)
        if errors: messagebox.showerror("Invalid spell","\n".join(errors)); return
        path=asksaveasfilename(defaultextension=".json",filetypes=[("Spell JSON","*.json")])
        if path: spell.save(__import__("pathlib").Path(path))

def run() -> None:
    root=tk.Tk(); SpellEditor(root); root.mainloop()