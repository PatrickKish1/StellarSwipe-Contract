# Trade executor

## Copy trade flow (`execute_copy_trade`)

`execute_copy_trade(user, token, amount)` runs, in order:

1. **`check_user_balance`** ([`risk_gates::check_user_balance`]) — reads `token::Client::balance(user)` and requires `balance >= amount + estimated_fee`. The fee term defaults to [`risk_gates::DEFAULT_ESTIMATED_COPY_TRADE_FEE`] and can be overridden with admin `set_copy_trade_estimated_fee`.
2. **`check_position_limit`** — see below.
3. **`record_copy_position(user)`** on the configured portfolio contract.

If the balance check fails, the contract returns **`ContractError::InsufficientBalance`** and stores [`errors::InsufficientBalanceDetail`] `{ required, available }` under instance storage; read it with **`get_insufficient_balance_detail(user)`**. That entry is cleared after a successful `execute_copy_trade`.

## Copy trade position limit (`risk_gates.rs`)

[`risk_gates::check_position_limit`]:

1. Returns `Ok(())` if the user is on the admin **position-limit whitelist** (instance key `PositionLimitExempt(user) == true`).
2. Otherwise invokes **`get_open_position_count(user) -> u32`** on the configured **user portfolio** contract via `Env::invoke_contract`.
3. Returns `ContractError::PositionLimitReached` when `open_count >= MAX_POSITIONS_PER_USER` (default **20**).

The position limit runs **after** the balance check so failing balances do not trigger a portfolio cross-call.

### Portfolio contract ABI

- `get_open_position_count(user: Address) -> u32` — required for the limit check.
- `record_copy_position(user: Address)` — called after successful checks (void return). Your portfolio contract should persist the new open position here (or equivalent).

### Admin

- `set_user_portfolio` — portfolio contract address.
- `set_position_limit_exempt(user, exempt)` — per-user bypass of the cap.
- `set_copy_trade_estimated_fee` / `get_copy_trade_estimated_fee` — fee term used in balance checks (`amount + fee`).
