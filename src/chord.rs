leanfran::lean! {
    pub mod model;
    r#"
    inductive Modifier where
      | shift
      | control
      | alt
      | logo
      deriving DecidableEq, Repr

    def Modifier.bit : Modifier → Nat
      | .shift => 0
      | .control => 2
      | .alt => 3
      | .logo => 6

    def realModifiers : List String :=
      ["Shift", "Lock", "Control", "Mod1", "Mod2", "Mod3", "Mod4", "Mod5"]

    def Modifier.xkbName : Modifier → String
      | .shift => "Shift"
      | .control => "Control"
      | .alt => "Mod1"
      | .logo => "Mod4"

    def mask : List Modifier → Nat
      | [] => 0
      | m :: ms => 2 ^ m.bit ||| mask ms

    def piecesFrom (cur : List Char) : List Char → List (List Char)
      | [] => [cur]
      | c :: cs => if c = '+' then cur :: piecesFrom [] cs else piecesFrom (cur ++ [c]) cs

    def pieces (text : List Char) : List (List Char) := piecesFrom [] text

    def join : List (List Char) → List Char
      | [] => []
      | [p] => p
      | p :: q :: r => p ++ '+' :: join (q :: r)

    def chord (text : List Char) : Option (List (List Char) × List Char) :=
      let ps := pieces text
      if ps.any List.isEmpty then none
      else match ps.getLast? with
        | some key => some (ps.dropLast, key)
        | none => none

    theorem bit_injective (a b : Modifier) (h : a.bit = b.bit) : a = b := by
      cases a <;> cases b <;> simp_all [Modifier.bit]

    theorem bit_xkbName (m : Modifier) : realModifiers[m.bit]? = some m.xkbName := by
      cases m <;> rfl

    theorem testBit_mask (ms : List Modifier) (i : Nat) :
        (mask ms).testBit i = ms.any (fun m => decide (m.bit = i)) := by
      induction ms with
      | nil => simp [mask]
      | cons m ms ih => simp [mask, Nat.testBit_or, Nat.testBit_two_pow, ih]

    theorem piecesFrom_ne_nil (cur text : List Char) : piecesFrom cur text ≠ [] := by
      induction text generalizing cur with
      | nil => simp [piecesFrom]
      | cons c cs ih =>
        unfold piecesFrom
        split
        · simp
        · exact ih _

    theorem join_piecesFrom (cur text : List Char) : join (piecesFrom cur text) = cur ++ text := by
      induction text generalizing cur with
      | nil => simp [piecesFrom, join]
      | cons c cs ih =>
        unfold piecesFrom
        split
        · next h =>
          subst h
          obtain ⟨q, r, e⟩ : ∃ q r, piecesFrom [] cs = q :: r := by
            cases hp : piecesFrom [] cs with
            | nil => exact absurd hp (piecesFrom_ne_nil _ _)
            | cons q r => exact ⟨q, r, rfl⟩
          have := ih []
          rw [e] at this ⊢
          simp [join, this]
        · rw [ih]; simp

    theorem plus_not_mem_piecesFrom (cur text : List Char) (h : '+' ∉ cur) :
        ∀ p ∈ piecesFrom cur text, '+' ∉ p := by
      induction text generalizing cur with
      | nil => simp [piecesFrom]; exact h
      | cons c cs ih =>
        unfold piecesFrom
        split
        · intro p hp
          simp only [List.mem_cons] at hp
          rcases hp with rfl | hp
          · exact h
          · exact ih [] (by simp) p hp
        · next hc => exact ih _ (by simp; exact ⟨h, fun e => hc e.symm⟩)

    theorem join_pieces (text : List Char) : join (pieces text) = text := by
      simp [pieces, join_piecesFrom]

    theorem dropLast_append_getLast? (l : List (List Char)) (k : List Char) (h : l.getLast? = some k) :
        l.dropLast ++ [k] = l := by
      have hne : l ≠ [] := by intro e; subst e; simp at h
      have : l.getLast hne = k := by
        rw [List.getLast?_eq_some_getLast hne] at h; exact Option.some.inj h
      rw [← this]; exact List.dropLast_concat_getLast hne

    theorem chord_some (text : List Char) :
        (chord text).all (fun (mods, key) =>
          join (mods ++ [key]) == text && (mods ++ [key]).all (fun p => !p.isEmpty && !p.contains '+')) := by
      unfold chord
      simp only
      split
      · rfl
      · next hany =>
        split
        · next k hk =>
          have e := dropLast_append_getLast? _ _ hk
          simp only [Option.all_some, e, join_pieces, beq_self_eq_true, Bool.true_and,
            List.all_eq_true, Bool.and_eq_true, Bool.not_eq_true', List.isEmpty_eq_false_iff]
          intro p hp
          refine ⟨?_, ?_⟩
          · intro hp0; subst hp0
            apply hany
            simp only [List.any_eq_true]
            exact ⟨[], hp, rfl⟩
          · have := plus_not_mem_piecesFrom [] text (by simp) p hp
            simpa using this
        · rfl

    theorem chord_none (text : List Char) : chord text = none ↔ [] ∈ pieces text := by
      unfold chord
      simp only
      split
      · next h =>
        simp only [List.any_eq_true, List.isEmpty_iff] at h
        obtain ⟨p, hp, rfl⟩ := h
        simp [hp]
      · next h =>
        simp only [List.any_eq_true, List.isEmpty_iff, not_exists, not_and] at h
        have hne : (pieces text).getLast? ≠ none := by
          simp [pieces, piecesFrom_ne_nil]
        constructor
        · intro hn
          split at hn
          · contradiction
          · exact absurd ‹_› hne
        · intro hm; exact absurd rfl (h [] hm)
    "#
}

pub use model::Modifier;

#[derive(Debug, PartialEq)]
pub enum Key {
    Char(char),
    Named(String),
}

pub struct Chord {
    pub mask: u32,
    pub key: Key,
}

fn modifier(name: &str) -> Option<Modifier> {
    match name.to_ascii_lowercase().as_str() {
        "shift" => Some(Modifier::Shift),
        "ctrl" | "control" => Some(Modifier::Control),
        "alt" | "mod1" => Some(Modifier::Alt),
        "super" | "logo" | "mod4" => Some(Modifier::Logo),
        _ => None,
    }
}

fn named_key(name: &str) -> Option<Key> {
    let lower = name.to_ascii_lowercase();
    let character = match lower.as_str() {
        "return" | "enter" => Some('\n'),
        "tab" => Some('\t'),
        "escape" | "esc" => Some('\u{1b}'),
        "backspace" => Some('\u{8}'),
        "space" => Some(' '),
        "plus" => Some('+'),
        _ => None,
    };
    if let Some(c) = character {
        return Some(Key::Char(c));
    }
    let known = match lower.as_str() {
        "delete" | "del" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "page_up" | "pageup" => "Page_Up",
        "page_down" | "pagedown" => "Page_Down",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "menu" => "Menu",
        "print" => "Print",
        _ => {
            return lower
                .strip_prefix('f')
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (1..=24).contains(n) && lower == format!("f{n}"))
                .map(|n| Key::Named(format!("F{n}")));
        }
    };
    Some(Key::Named(known.to_string()))
}

fn key(name: &str) -> Result<Key, String> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(Key::Char(c));
    }
    named_key(name).ok_or_else(|| {
        format!(
            "unknown key {name:?}; use one character or Return, Tab, Escape, BackSpace, Delete, \
             Insert, Home, End, Page_Up, Page_Down, Left, Right, Up, Down, space, plus, Menu, \
             Print or F1 to F24"
        )
    })
}

pub fn parse(text: &str) -> Result<Chord, String> {
    let (modifiers, key) = model::chord(text.chars().collect()).ok_or_else(|| {
        format!("{text:?} has an empty part; write keys such as Return, ctrl+l or alt+Tab, and plus for the + key")
    })?;
    let modifiers = modifiers
        .iter()
        .map(|part| {
            let name: String = part.iter().collect();
            modifier(&name)
                .ok_or_else(|| format!("unknown modifier {name:?}; use shift, ctrl, alt or super"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let name: String = key.iter().collect();
    let key = self::key(&name)?;
    let mask = u64::try_from(model::mask(modifiers)).expect("a mask below 2^7") as u32;
    Ok(Chord { mask, key })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap;

    #[test]
    fn modifiers_become_real_modifier_bits() {
        let chord = parse("ctrl+l").unwrap();
        assert_eq!((chord.mask, chord.key), (4, Key::Char('l')));
        let chord = parse("Shift+Alt+super+ctrl+Tab").unwrap();
        assert_eq!((chord.mask, chord.key), (1 | 4 | 8 | 64, Key::Char('\t')));
        let chord = parse("Return").unwrap();
        assert_eq!((chord.mask, chord.key), (0, Key::Char('\n')));
        assert_eq!(parse("ctrl+plus").unwrap().key, Key::Char('+'));
        assert_eq!(parse("F12").unwrap().key, Key::Named("F12".to_string()));
    }

    #[test]
    fn every_named_key_has_a_key_in_the_keymap() {
        for name in [
            "Delete",
            "Insert",
            "Home",
            "End",
            "Page_Up",
            "Page_Down",
            "Left",
            "Right",
            "Up",
            "Down",
            "Menu",
            "Print",
            "F1",
            "F24",
        ] {
            let Key::Named(keysym) = parse(name).unwrap().key else {
                panic!("{name} is a character");
            };
            assert!(keymap::named_key(&keysym).is_some(), "{name} has no key");
        }
    }

    #[test]
    fn rejects_empty_parts_and_unknown_names() {
        for bad in [
            "", "ctrl+", "+l", "ctrl++", "hyper+l", "F25", "F01", "Retrun",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} was accepted");
        }
    }
}
