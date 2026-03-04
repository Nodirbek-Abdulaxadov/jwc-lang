CREATE TABLE IF NOT EXISTS "brands" (
  "id"   SERIAL PRIMARY KEY,
  "name" VARCHAR(255) NOT NULL
);

CREATE TABLE IF NOT EXISTS "colors" (
  "id"   SERIAL PRIMARY KEY,
  "name" VARCHAR(255) NOT NULL,
  "hex"  VARCHAR(7)   NOT NULL
);

CREATE TABLE IF NOT EXISTS "cars" (
  "id"       SERIAL PRIMARY KEY,
  "model"    VARCHAR(255) NOT NULL,
  "year"     INTEGER      NOT NULL,
  "brandId"  INTEGER      NOT NULL REFERENCES "brands"("id"),
  "colorId"  INTEGER      NOT NULL REFERENCES "colors"("id")
);
