# Notes 9: pretraining and finetuning

Masked language modeling (BERT) versus autoregressive pretraining
(GPT). Finetuning adapts the whole network; prompting and lightweight
adapters adapt without touching most weights. Scaling laws: loss
falls predictably with compute, data, and parameters. The assignment
finetunes a small pretrained transformer for question answering.
