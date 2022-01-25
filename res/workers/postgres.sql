-- Setup a user and a database.

CREATE USER synapse PASSWORD 'password';
CREATE DATABASE synapse OWNER = 'synapse' ENCODING 'UTF8' LC_COLLATE='C' LC_CTYPE='C' template=template0;

-- Create user if not exists.
--SELECT E'CREATE USER synapse PASSWORD \'password\''
--WHERE NOT EXISTS (SELECT FROM pg_user WHERE usename = 'synapse');

-- Create database if not exists.
--SELECT E'CREATE DATABASE synapse OWNER = \'synapse\' ENCODING \'UTF8\' LC_COLLATE=\'C\' LC_CTYPE=\'C\' template=template0'
--WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'synapse')\gexec

