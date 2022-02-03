-- Setup a user and a database.
CREATE USER synapse PASSWORD 'password';
CREATE DATABASE synapse OWNER = 'synapse' ENCODING 'UTF8' LC_COLLATE='C' LC_CTYPE='C' template=template0;
