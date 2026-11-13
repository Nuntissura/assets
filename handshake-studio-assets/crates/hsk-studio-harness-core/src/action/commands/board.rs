//! Board rows of the command corpus (lane SMB-BOARD-1; [STU-WB-134] node-factory, read and mutate commands
//! per board node kind, [STU-WB-001], [STU-WB-002], [STU-WB-104] to [STU-WB-108], [STU-WB-115], [STU-WB-127]):
//! every row runs `Viewer::board_command`; each mutating row is one named undo step. Kinds of this slice:
//! sticky, shape_with_text, section, connector.

use super::{CommandEntry, ParamSpec, flag, number as n, one_of as o, output_path, record as r, rgb8, text as t, typed};

const METHOD: &str = "Viewer::board_command";
const LINE_TYPES: &str = "STRAIGHT|ELBOWED|CURVED";
const CAPS: &str = "NONE|ARROW_EQUILATERAL|ARROW_LINES|TRIANGLE_FILLED|CIRCLE_FILLED|DIAMOND_FILLED|ERD_ONE|ERD_MANY|ERD_ONE_OR_MORE|ERD_ZERO_OR_ONE|ERD_ZERO_OR_MORE|ERD_EXACTLY_ONE";
const ENDPOINT: &str = "{position: {x, y}} | {endpoint_node_id, magnet: AUTO|TOP|RIGHT|BOTTOM|LEFT|CENTER} | {position: {x, y}, endpoint_node_id}";
const BOARD: ParamSpec = t("board").opt();
const NODE: ParamSpec = t("node");
const START: ParamSpec = r("start", ENDPOINT);
const END: ParamSpec = r("end", ENDPOINT);

pub(super) const ROWS: &[CommandEntry] = &[
    typed("board.add", METHOD, "New board", true, &[t("name").opt(), n("width").opt(), n("height").opt(), n("x").opt(), n("y").opt()]),
    typed("board.read", METHOD, "Read board", false, &[BOARD]),
    typed("board.node.read", METHOD, "Read board node", false, &[NODE]),
    typed("board.sticky.create", METHOD, "New sticky", true, &[BOARD, n("x"), n("y"), t("text"), flag("author_visible").opt(), t("author_name").opt(), flag("is_wide_width").opt(), rgb8("fill").opt()]),
    typed("board.sticky.set", METHOD, "Sticky options", true, &[NODE, flag("author_visible").opt(), t("author_name").opt(), flag("is_wide_width").opt()]),
    typed("board.shape_with_text.create", METHOD, "New shape with text", true, &[BOARD, n("x"), n("y"), t("shape_type"), t("text"), n("width").opt(), n("height").opt(), n("corner_radius").opt(), rgb8("fill").opt()]),
    typed("board.shape_with_text.set", METHOD, "Shape options", true, &[NODE, t("shape_type").opt(), n("corner_radius").opt()]),
    typed("board.section.create", METHOD, "New section", true, &[BOARD, n("x"), n("y"), n("width"), n("height"), t("name"), n("corner_radius").opt(), rgb8("fill").opt()]),
    typed("board.section.set", METHOD, "Section options", true, &[NODE, t("name").opt(), flag("section_contents_hidden").opt(), o("dev_status", "READY_FOR_DEV|COMPLETED|NONE").opt()]),
    typed("board.connector.create", METHOD, "New connector", true, &[BOARD, START, END, o("connector_line_type", LINE_TYPES).opt(), o("connector_start_stroke_cap", CAPS).opt(), o("connector_end_stroke_cap", CAPS).opt(), rgb8("stroke").opt(), n("stroke_width").opt()]),
    typed("board.connector.set", METHOD, "Connector options", true, &[NODE, START.opt(), END.opt(), o("connector_line_type", LINE_TYPES).opt(), o("connector_start_stroke_cap", CAPS).opt(), o("connector_end_stroke_cap", CAPS).opt()]),
    typed("board.node.set_text", METHOD, "Edit board text", true, &[NODE, t("text")]),
    typed("board.node.set_fill", METHOD, "Board node colour", true, &[NODE, rgb8("fill")]),
    typed("board.node.move", METHOD, "Move board node", true, &[NODE, n("x"), n("y")]),
    typed("board.node.delete", METHOD, "Delete board node", true, &[NODE]),
    typed("board.export_png", METHOD, "Export board as PNG", false, &[t("board").opt(), output_path("path"), flag("overwrite").opt(), o("bounds", "content|board").opt()]),
];
