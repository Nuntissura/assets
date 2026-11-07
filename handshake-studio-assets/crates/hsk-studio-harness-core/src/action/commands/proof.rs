//! Proof rows of the command corpus: every `ProofCommand`.

use super::{CommandEntry, flag as b, index as i, one_of as o, record as r, text as t, typed, vector3};

const PROOF: &str = "Viewer::proof_command";

pub(super) const ROWS: &[CommandEntry] = &[
    typed("proof.select", PROOF, "Select proof condition", true, &[t("name")]),
    typed("proof.custom", PROOF, "Custom proof condition", true, &[r("condition", "a Folio StudioProofConditionRecord")]),
    typed("proof.set_proof_colours", PROOF, "Proof colours", true, &[b("on")]),
    typed("proof.set_gamut_warning", PROOF, "Gamut warning", true, &[b("on")]),
    typed("proof.set_gamut_style", PROOF, "Gamut warning style", true, &[vector3("colour"), i("opacity")]),
    typed("proof.select_colour_vision", PROOF, "Colour-vision proof", true, &[o("vision", "protanopia|deuteranopia|tritanopia")]),
    typed("proof.save_preset", PROOF, "Save proof preset", true, &[t("name")]),
];
