# CS230 project milestone: bike-share demand, deep version

Continuing the CS229 project data. The feedforward baseline matches
gradient boosting only after batch norm and Adam with warmup.
Added an LSTM over the previous 24 hours of rentals: RMSE 54.1 vs
58.9 for the tabular network. Error analysis says holidays dominate
the residual; next step is a holiday embedding.
