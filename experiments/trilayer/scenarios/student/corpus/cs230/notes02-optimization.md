# Notes 2: optimization beyond vanilla gradient descent

Mini-batch gradient descent with momentum smooths the update
direction; RMSprop rescales per-parameter step sizes; Adam combines
both with bias correction. Learning-rate warmup then decay is the
default recipe. The professor's rule of thumb: tune the learning
rate first, everything else second. CS229 derived plain gradient
descent; this lecture is about making it actually converge fast.
