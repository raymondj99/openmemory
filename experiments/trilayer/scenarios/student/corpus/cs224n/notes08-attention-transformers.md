# Notes 8: attention and transformers

Attention replaces recurrence with direct weighted access to all
positions: queries, keys, values, scaled dot products, softmax over
scores. Multi-head attention looks at different subspaces. The
transformer block stacks attention with feedforward layers, residual
connections, and layer normalization. Positional encodings restore
order information. This is the architecture every pretrained model
in the rest of the course builds on.
