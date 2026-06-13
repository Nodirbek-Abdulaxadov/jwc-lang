-- Initial schema for {{name}} (auth template).
CREATE TABLE IF NOT EXISTS "user_entity" (
    "id"            serial       NOT NULL,
    "email"         varchar(120) NOT NULL,
    "passwordHash"  varchar(255) NOT NULL,
    "createdAt"     timestamp    NOT NULL DEFAULT now(),
    PRIMARY KEY ("id"),
    UNIQUE ("email")
);
