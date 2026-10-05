\set ON_ERROR_STOP on
BEGIN;
CREATE TABLE public.products (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL,
    price numeric(12, 2) NOT NULL CHECK (price >= 0),
    stock integer NOT NULL CHECK (stock >= 0),
    active boolean NOT NULL
);
INSERT INTO public.products (name, category, price, stock, active) VALUES
    ('Keyboard', 'Accessories', 49.90, 25, true),
    ('Monitor', 'Displays', 249.00, 8, true),
    ('USB-C cable', 'Accessories', 12.50, 100, true),
    ('Laptop stand', 'Accessories', 39.00, 0, false),
    ('Mouse', 'Accessories', 29.90, 40, true);
CREATE ROLE wes_reader LOGIN;
GRANT CONNECT ON DATABASE wes_demo TO wes_reader;
GRANT USAGE ON SCHEMA public TO wes_reader;
GRANT SELECT ON public.products TO wes_reader;
COMMIT;
