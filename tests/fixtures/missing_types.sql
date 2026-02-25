CREATE TABLE type_compat_test (
    id         BIGINT        PRIMARY KEY,
    label      NVARCHAR(100) NOT NULL,
    notes      CLOB,
    amount     NUMERIC(10, 2),
    ratio      DOUBLE PRECISION,
    raw_bytes  VARBINARY(256),
    flag       BIT,
    created    DATE,
    active     BOOLEAN       NOT NULL DEFAULT true
);
