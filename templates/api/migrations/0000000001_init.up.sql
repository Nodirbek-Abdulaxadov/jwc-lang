-- Initial schema for {{name}}.
-- Regenerate / extend with `jwc migrate new <name>` after editing entities.
CREATE TABLE IF NOT EXISTS "item_entity" (
    "id"        serial      NOT NULL,
    "name"      varchar(255) NOT NULL,
    "createdAt" timestamp    NOT NULL DEFAULT now(),
    PRIMARY KEY ("id")
);
