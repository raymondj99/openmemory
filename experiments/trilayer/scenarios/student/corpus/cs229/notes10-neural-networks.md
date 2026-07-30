# Notes 10: neural networks and backpropagation

Backpropagation is just the chain rule organized so each local
gradient is computed once. We derived the two-layer case by hand;
the vectorized form uses the same softmax gradient from notes 3.
Initialization matters: symmetric weights never break symmetry.
CS230 goes much deeper on optimization tricks; these notes only
cover vanilla gradient descent.
