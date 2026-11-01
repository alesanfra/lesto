CREATE TABLE notes (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    text TEXT NOT NULL CHECK (length(text) BETWEEN 1 AND 280)
);

INSERT INTO notes (text) VALUES ('Hello from D1');
