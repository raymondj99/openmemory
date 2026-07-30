# Notes 8: recurrent networks and LSTMs

Recurrent networks share weights across time; backpropagation
through time unrolls the graph. Vanishing gradients motivate gates:
the LSTM cell state carries information with additive updates, the
GRU merges gates for a cheaper cell. For long sequences attention
now outperforms pure recurrence; CS224N covers that shift in detail.
