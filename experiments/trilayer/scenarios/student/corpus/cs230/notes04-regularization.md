# Notes 4: regularization in deep networks

L2 weight decay, dropout as an ensemble approximation, batch
normalization's regularizing side effect, early stopping against the
dev-set curve, and data augmentation. Dropout at test time is
replaced by weight scaling. Batch norm changes the loss landscape
enough that it interacts badly with small batch sizes; use layer
norm there, which is also what transformers use (cs224n/notes08-attention-transformers.md).
