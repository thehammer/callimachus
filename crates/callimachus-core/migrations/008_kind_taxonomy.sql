CREATE TABLE kind_taxonomy (
    concrete_kind TEXT NOT NULL,
    corpus_kind   TEXT NOT NULL,
    abstract_kind TEXT NOT NULL,
    PRIMARY KEY (concrete_kind, corpus_kind)
);

INSERT INTO kind_taxonomy VALUES
    ('function',  'code', 'process'),
    ('class',     'code', 'component'),
    ('module',    'code', 'component'),
    ('interface', 'code', 'component'),
    ('character', 'book', 'person'),
    ('location',  'book', 'place'),
    ('faction',   'book', 'organization'),
    ('concept',   'book', 'concept');

ALTER TABLE entities ADD COLUMN abstract_kind TEXT NOT NULL DEFAULT '';

UPDATE entities SET abstract_kind = COALESCE((
    SELECT kt.abstract_kind FROM kind_taxonomy kt
    JOIN corpora c ON c.id = entities.corpus_id
    WHERE kt.concrete_kind = entities.kind AND kt.corpus_kind = c.kind
), '') WHERE abstract_kind = '';
