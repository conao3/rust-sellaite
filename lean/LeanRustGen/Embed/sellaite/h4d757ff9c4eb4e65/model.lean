namespace model

def addSlot (acc : List Char) (c : Char) : List Char :=
  if c ∈ acc then acc else acc ++ [c]

def slotsFrom (acc : List Char) : List Char → List Char
  | [] => acc
  | c :: cs => slotsFrom (addSlot acc c) cs

def slots (text : List Char) : List Char := slotsFrom [] text

def position (c : Char) : List Char → Option Nat
  | [] => none
  | s :: ss => if s = c then some 0 else (position c ss).map (· + 1)

def evdevKey (slots : List Char) (c : Char) : Option Nat := (position c slots).map (· + 1)

def chunksFrom (cap : Nat) (cur : List Char) (seen : List Char) : List Char → List (List Char)
  | [] => if cur = [] then [] else [cur]
  | c :: cs =>
    if c ∈ seen then chunksFrom cap (cur ++ [c]) seen cs
    else if seen.length < cap then chunksFrom cap (cur ++ [c]) (seen ++ [c]) cs
    else cur :: chunksFrom cap [c] [c] cs

def split (cap : Nat) (text : List Char) : List (List Char) := chunksFrom cap [] [] text

def chained (cap : Nat) : List (List Char) → Bool
  | p :: q :: r =>
    (slots p).length == cap && (q.head?.all fun c => !p.contains c) && chained cap (q :: r)
  | _ => true

theorem mem_addSlot (acc : List Char) (c d : Char) : d ∈ addSlot acc c ↔ d ∈ acc ∨ d = c := by
  unfold addSlot; split <;> simp_all

theorem nodup_addSlot (acc : List Char) (c : Char) (h : acc.Nodup) : (addSlot acc c).Nodup := by
  unfold addSlot; split
  · exact h
  · rw [List.nodup_append]; simp_all
    intro a ha hac; subst hac; contradiction

theorem mem_slotsFrom (acc text : List Char) (d : Char) :
    d ∈ slotsFrom acc text ↔ d ∈ acc ∨ d ∈ text := by
  induction text generalizing acc with
  | nil => simp [slotsFrom]
  | cons c cs ih => simp only [slotsFrom, ih, mem_addSlot, List.mem_cons, or_assoc]

theorem nodup_slotsFrom (acc text : List Char) (h : acc.Nodup) : (slotsFrom acc text).Nodup := by
  induction text generalizing acc with
  | nil => exact h
  | cons c cs ih => exact ih _ (nodup_addSlot acc c h)

theorem position_isSome (c : Char) (l : List Char) : (position c l).isSome ↔ c ∈ l := by
  induction l with
  | nil => simp [position]
  | cons s ss ih =>
    by_cases h : s = c
    · simp [position, h]
    · simp only [position, h, ite_false, Option.isSome_map, ih, List.mem_cons]
      constructor
      · exact Or.inr
      · rintro (h' | h')
        · exact absurd h'.symm h
        · exact h'

theorem position_get (c : Char) (l : List Char) :
    (position c l).all (fun i => l[i]? == some c) := by
  induction l with
  | nil => simp [position]
  | cons s ss ih =>
    by_cases h : s = c
    · simp [position, h]
    · simp only [position, h, ite_false]
      cases hp : position c ss <;> simp_all

theorem evdevKey_isSome (text : List Char) (c : Char) :
    (evdevKey (slots text) c).isSome ↔ c ∈ text := by
  simp [evdevKey, position_isSome, slots, mem_slotsFrom]

theorem evdevKey_slot (text : List Char) (c : Char) :
    (evdevKey (slots text) c).all (fun k => 0 < k && (slots text)[k - 1]? == some c) := by
  have := position_get c (slots text)
  unfold evdevKey
  cases h : position c (slots text) <;> simp_all

theorem slots_nodup (text : List Char) : (slots text).Nodup :=
  nodup_slotsFrom [] text List.nodup_nil


theorem slotsFrom_snoc (acc l : List Char) (c : Char) :
    slotsFrom acc (l ++ [c]) = addSlot (slotsFrom acc l) c := by
  induction l generalizing acc with
  | nil => rfl
  | cons d ds ih => exact ih _

theorem slots_snoc (l : List Char) (c : Char) : slots (l ++ [c]) = addSlot (slots l) c :=
  slotsFrom_snoc [] l c

theorem flatten_chunksFrom (cap : Nat) (cur seen text : List Char) :
    (chunksFrom cap cur seen text).flatten = cur ++ text := by
  induction text generalizing cur seen with
  | nil => unfold chunksFrom; split <;> simp_all
  | cons c cs ih =>
    unfold chunksFrom
    split
    · rw [ih]; simp
    · split
      · rw [ih]; simp
      · simp [ih]

theorem flatten_split (cap : Nat) (text : List Char) : (split cap text).flatten = text := by
  simp [split, flatten_chunksFrom]

theorem chunksFrom_spec (cap : Nat) (hcap : 0 < cap) (cs cur seen : List Char)
    (hseen : seen = slots cur) (hle : seen.length ≤ cap) :
    (∀ p ∈ chunksFrom cap cur seen cs, p ≠ [] ∧ (slots p).length ≤ cap) ∧
    chained cap (chunksFrom cap cur seen cs) = true ∧
    (cur ≠ [] → ∃ t r, chunksFrom cap cur seen cs = (cur ++ t) :: r) := by
  induction cs generalizing cur seen with
  | nil =>
    unfold chunksFrom
    by_cases h : cur = []
    · simp [h, chained]
    · simp only [h, ite_false]
      refine ⟨?_, by simp [chained], fun _ => ⟨[], [], by simp⟩⟩
      intro p hp; simp at hp; subst hp; exact ⟨h, hseen ▸ hle⟩
  | cons c cs ih =>
    unfold chunksFrom
    by_cases hc : c ∈ seen
    · simp only [hc, ite_true]
      have hs : seen = slots (cur ++ [c]) := by rw [slots_snoc, ← hseen]; simp [addSlot, hc]
      obtain ⟨h1, h2, h3⟩ := ih (cur ++ [c]) seen hs hle
      refine ⟨h1, h2, fun _ => ?_⟩
      obtain ⟨t, r, e⟩ := h3 (by simp)
      exact ⟨[c] ++ t, r, by simpa using e⟩
    · simp only [hc, ite_false]
      by_cases hl : seen.length < cap
      · simp only [hl, ite_true]
        have hs : seen ++ [c] = slots (cur ++ [c]) := by rw [slots_snoc, ← hseen]; simp [addSlot, hc]
        obtain ⟨h1, h2, h3⟩ := ih (cur ++ [c]) (seen ++ [c]) hs (by simp; omega)
        refine ⟨h1, h2, fun _ => ?_⟩
        obtain ⟨t, r, e⟩ := h3 (by simp)
        exact ⟨[c] ++ t, r, by simpa using e⟩
      · simp only [hl, ite_false]
        have hs : [c] = slots [c] := by simp [slots, slotsFrom, addSlot]
        obtain ⟨h1, h2, h3⟩ := ih [c] [c] hs (by simp; omega)
        have hcur : cur ≠ [] := by
          intro e; apply hl; rw [hseen, e]; simp [slots, slotsFrom]; exact hcap
        obtain ⟨t, r, e⟩ := h3 (by simp)
        refine ⟨?_, ?_, fun _ => ⟨[], chunksFrom cap [c] [c] cs, by simp⟩⟩
        · intro p hp
          simp only [List.mem_cons] at hp
          rcases hp with rfl | hp
          · exact ⟨hcur, by rw [← hseen]; omega⟩
          · exact h1 p hp
        · rw [e]; rw [e] at h2
          have hcs : c ∉ cur := by
            intro m; apply hc; rw [hseen]; exact (mem_slotsFrom [] cur c).2 (Or.inr m)
          simp only [chained, ← hseen, Bool.and_eq_true, beq_iff_eq]
          refine ⟨⟨by omega, ?_⟩, h2⟩
          simp [hcs]

theorem split_fits (cap : Nat) (text : List Char) (hcap : 0 < cap) :
    (split cap text).all (fun p => !p.isEmpty && (slots p).length ≤ cap) := by
  have := (chunksFrom_spec cap hcap text [] [] (by simp [slots, slotsFrom]) (by simp)).1
  simp only [List.all_eq_true, Bool.and_eq_true, Bool.not_eq_true', List.isEmpty_eq_false_iff,
    decide_eq_true_eq]
  exact fun p hp => this p hp

theorem split_chained (cap : Nat) (text : List Char) (hcap : 0 < cap) :
    chained cap (split cap text) = true :=
  (chunksFrom_spec cap hcap text [] [] (by simp [slots, slotsFrom]) (by simp)).2.1

end model
