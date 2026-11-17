-- The mode of the assistant's CLI chosen for the conversation, as the CLI
-- names it (acceptEdits, plan…); none for the assistant's default.
ALTER TABLE conversation ADD COLUMN mode TEXT;
