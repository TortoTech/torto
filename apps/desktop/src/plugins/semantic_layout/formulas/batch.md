# Batch protocol

This request contains multiple independent images. The single-image transcription
and verification rules above apply separately to every supplied `image_id`.

## Input
Each item starts with a text label containing its `image_id` and context, followed
by its original image. In verification mode, the original is immediately followed
by that item's proposed rendering. Never mix symbols or equation numbers between
items. Only inspect the images attached to the same ID.

## Output
Use the batch JSON Schema: `{"results":[{"image_id":0,"status":"recognized",
"latex":"x=1","equation_number":null}]}`.
Return exactly one result for EACH requested ID, including negative/unreadable
results. IDs need not be consecutive on a retry. Do not invent IDs or omit items.
The client matches results by ID, never by array position.
