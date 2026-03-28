#![no_std]

use core::convert::TryFrom;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, IntoVal, String,
    Symbol, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    EventNotFound = 2,
    EventAlreadyCanceled = 3,
    CannotSellMoreTickets = 4,
    InvalidStartDate = 5,
    InvalidEndDate = 6,
    NegativeTicketPrice = 7,
    InvalidTicketCount = 8,
    CounterOverflow = 9,
    FactoryNotInitialized = 10,
    InvalidTierIndex = 11,
    TierSoldOut = 12,
    InvalidTierConfig = 13,
    EventNotCanceled = 14,
    RefundAlreadyClaimed = 15,
    NotABuyer = 16,
    EventSoldOut = 17,
    TicketsBelowSold = 18,
    /// Returned by withdraw_funds when the event end date has not yet passed
    EventNotEnded = 19,
    /// Returned by withdraw_funds when funds have already been withdrawn
    FundsAlreadyWithdrawn = 20,
    /// Returned by join_waitlist when the event is not yet sold out
    EventNotSoldOut = 21,
    /// Returned by join_waitlist when the caller is already on the waitlist
    AlreadyOnWaitlist = 22,
    /// Returned by join_waitlist when the event is cancelled
    EventCanceled = 23,
    /// Returned when a discount code does not exist for the event
    DiscountNotFound = 24,
    /// Returned when a discount code's expiration timestamp has passed
    DiscountExpired = 25,
    /// Returned when a discount code has no remaining uses
    DiscountMaxUsesReached = 26,
    /// Returned when percentage is outside the valid range (1–100)
    InvalidDiscountPercentage = 27,
    /// Returned when creating a code that already exists for the event
    DiscountAlreadyExists = 28,
    /// Returned when a discount expiration is set in the past
    InvalidExpiration = 29,
}

#[contracttype]
pub enum DataKey {
    Event(u32),
    EventCounter,
    TicketFactory,
    RefundClaimed(u32, Address),
    EventBuyers(u32),
    /// event_id -> Vec<TicketTier>
    EventTiers(u32),
    BuyerPurchase(u32, Address),
    /// Escrowed ticket sale balance held by the contract until withdrawal
    EventBalance(u32),
    /// Set to `true` once the organizer has withdrawn funds for an event
    FundsWithdrawn(u32),
    /// Ordered waitlist of addresses for a sold-out event (Vec<Address>)
    Waitlist(u32),
    /// (event_id, code) -> DiscountCode
    Discount(u32, String),
}

/// A single ticket tier (e.g. VIP, General, Early Bird)
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TicketTier {
    pub name: String,
    pub price: i128,
    pub total_quantity: u128,
    pub sold_quantity: u128,
}

/// Input config for creating a tier
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TierConfig {
    pub name: String,
    pub price: i128,
    pub total_quantity: u128,
}

/// Parameters for creating a new event
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateEventParams {
    pub organizer: Address,
    pub theme: String,
    pub event_type: String,
    pub start_date: u64,
    pub end_date: u64,
    pub ticket_price: i128,
    pub total_tickets: u128,
    pub payment_token: Address,
    pub tiers: Vec<TierConfig>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    pub id: u32,
    pub theme: String,
    pub organizer: Address,
    pub event_type: String,
    pub total_tickets: u128,
    pub tickets_sold: u128,
    pub ticket_price: i128,
    pub start_date: u64,
    pub end_date: u64,
    pub is_canceled: bool,
    pub ticket_nft_addr: Address,
    pub payment_token: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuyerPurchase {
    pub quantity: u128,
    pub total_paid: i128,
}

/// A promotional discount code attached to a specific event.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscountCode {
    /// The code string (mirrors the key for convenient querying)
    pub code: String,
    /// Percentage discount applied to the ticket price (1–100)
    pub percentage: u32,
    /// Maximum number of times this code may be used (0 = unlimited)
    pub max_uses: u32,
    /// How many uses are still available
    pub uses_remaining: u32,
    /// Unix timestamp after which the code is invalid (0 = no expiration)
    pub expiration: u64,
}

#[contract]
pub struct EventManager;

#[contractimpl]
impl EventManager {
    pub fn initialize(env: Env, ticket_factory: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::TicketFactory) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&DataKey::TicketFactory, &ticket_factory);
        env.storage().instance().set(&DataKey::EventCounter, &0u32);
        Ok(())
    }

    /// Create a new event.
    /// `params.tiers`: optional multi-tier config. If empty, falls back to a single
    /// "General" tier using `params.ticket_price` and `params.total_tickets` (backward-compatible).
    pub fn create_event(env: Env, params: CreateEventParams) -> Result<u32, Error> {
        params.organizer.require_auth();

        Self::validate_event_params(
            &env,
            params.start_date,
            params.end_date,
            params.ticket_price,
            params.total_tickets,
        )?;

        let resolved_tiers = if params.tiers.is_empty() {
            let mut v = Vec::new(&env);
            v.push_back(TicketTier {
                name: String::from_str(&env, "General"),
                price: params.ticket_price,
                total_quantity: params.total_tickets,
                sold_quantity: 0,
            });
            v
        } else {
            let mut v = Vec::new(&env);
            for cfg in params.tiers.iter() {
                if cfg.price < 0 {
                    return Err(Error::NegativeTicketPrice);
                }
                if cfg.total_quantity == 0 {
                    return Err(Error::InvalidTierConfig);
                }
                v.push_back(TicketTier {
                    name: cfg.name.clone(),
                    price: cfg.price,
                    total_quantity: cfg.total_quantity,
                    sold_quantity: 0,
                });
            }
            v
        };

        let agg_total: u128 = resolved_tiers.iter().map(|t| t.total_quantity).sum();
        let agg_price = resolved_tiers
            .first()
            .map(|t| t.price)
            .unwrap_or(params.ticket_price);

        let event_id = Self::get_and_increment_counter(&env)?;
        let ticket_nft_addr =
            Self::deploy_ticket_nft(&env, event_id).ok_or(Error::FactoryNotInitialized)?;

        let event = Event {
            id: event_id,
            theme: params.theme.clone(),
            organizer: params.organizer.clone(),
            event_type: params.event_type,
            total_tickets: agg_total,
            tickets_sold: 0,
            ticket_price: agg_price,
            start_date: params.start_date,
            end_date: params.end_date,
            is_canceled: false,
            ticket_nft_addr: ticket_nft_addr.clone(),
            payment_token: params.payment_token,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        env.storage()
            .persistent()
            .set(&DataKey::EventTiers(event_id), &resolved_tiers);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));
        Self::extend_persistent_ttl(&env, &DataKey::EventTiers(event_id));
        env.storage()
            .instance()
            .extend_ttl(Self::ttl_threshold(), Self::ttl_extend_to());

        env.events().publish(
            (Symbol::new(&env, "event_created"),),
            (event_id, params.organizer, ticket_nft_addr),
        );

        Ok(event_id)
    }

    pub fn get_event(env: Env, event_id: u32) -> Result<Event, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)
    }

    /// Get tiers for an event
    pub fn get_event_tiers(env: Env, event_id: u32) -> Result<Vec<TicketTier>, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::EventTiers(event_id))
            .ok_or(Error::EventNotFound)
    }

    /// Get total number of events
    pub fn get_event_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::EventCounter)
            .unwrap_or(0)
    }

    /// Get all events
    pub fn get_all_events(env: Env) -> Vec<Event> {
        let count = Self::get_event_count(env.clone());
        let mut events = Vec::new(&env);
        for event_id in 0..count {
            if let Some(event) = env.storage().persistent().get(&DataKey::Event(event_id)) {
                events.push_back(event);
            }
        }
        events
    }

    pub fn get_buyer_purchase(env: Env, event_id: u32, buyer: Address) -> Option<BuyerPurchase> {
        env.storage()
            .persistent()
            .get(&DataKey::BuyerPurchase(event_id, buyer))
    }

    pub fn cancel_event(env: Env, event_id: u32) -> Result<(), Error> {
        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        event.organizer.require_auth();

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        event.is_canceled = true;
        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        env.events()
            .publish((Symbol::new(&env, "event_canceled"),), event_id);

        // Notify any waitlisted users that the event will not happen
        let waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Waitlist(event_id))
            .unwrap_or_else(|| Vec::new(&env));

        if !waitlist.is_empty() {
            env.events().publish(
                (Symbol::new(&env, "waitlist_cleared"),),
                (event_id, waitlist.len()),
            );
        }

        Ok(())
    }

    /// Claim refund for a canceled event (pull model).
    /// Funds are transferred from the contract's escrowed balance back to the buyer.
    pub fn claim_refund(env: Env, claimer: Address, event_id: u32) -> Result<(), Error> {
        claimer.require_auth();

        let event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        if !event.is_canceled {
            return Err(Error::EventNotCanceled);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::RefundClaimed(event_id, claimer.clone()))
        {
            return Err(Error::RefundAlreadyClaimed);
        }

        let purchase: BuyerPurchase = env
            .storage()
            .persistent()
            .get(&DataKey::BuyerPurchase(event_id, claimer.clone()))
            .ok_or(Error::NotABuyer)?;

        // Mark refund claimed before transfer (checks-effects-interactions)
        env.storage()
            .persistent()
            .set(&DataKey::RefundClaimed(event_id, claimer.clone()), &true);
        Self::extend_persistent_ttl(&env, &DataKey::RefundClaimed(event_id, claimer.clone()));

        if purchase.total_paid > 0 {
            let token_client = soroban_sdk::token::Client::new(&env, &event.payment_token);
            token_client.transfer(
                &env.current_contract_address(),
                &claimer,
                &purchase.total_paid,
            );

            // Deduct refunded amount from the escrowed balance
            let balance_key = DataKey::EventBalance(event_id);
            let current_balance: i128 = env
                .storage()
                .persistent()
                .get(&balance_key)
                .unwrap_or(0);
            env.storage()
                .persistent()
                .set(&balance_key, &current_balance.saturating_sub(purchase.total_paid));
        }

        env.events().publish(
            (Symbol::new(&env, "refund_claimed"),),
            (event_id, claimer, purchase.quantity, purchase.total_paid),
        );

        Ok(())
    }

    /// Update event details. Only callable by the organizer before the event is canceled.
    pub fn update_event(
        env: Env,
        event_id: u32,
        theme: Option<String>,
        ticket_price: Option<i128>,
        total_tickets: Option<u128>,
        start_date: Option<u64>,
        end_date: Option<u64>,
    ) -> Result<(), Error> {
        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        event.organizer.require_auth();

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        let current_time = env.ledger().timestamp();

        if let Some(next_theme) = theme {
            event.theme = next_theme;
        }

        if let Some(next_price) = ticket_price {
            if next_price < 0 {
                return Err(Error::NegativeTicketPrice);
            }
            event.ticket_price = next_price;
        }

        if let Some(next_total) = total_tickets {
            if next_total == 0 {
                return Err(Error::InvalidTicketCount);
            }
            if next_total < event.tickets_sold {
                return Err(Error::TicketsBelowSold);
            }
            event.total_tickets = next_total;
        }

        let effective_end = end_date.unwrap_or(event.end_date);
        if let Some(next_start) = start_date {
            if next_start < current_time {
                return Err(Error::InvalidStartDate);
            }
            if next_start >= effective_end {
                return Err(Error::InvalidEndDate);
            }
            event.start_date = next_start;
        }

        let effective_start = start_date.unwrap_or(event.start_date);
        if let Some(next_end) = end_date {
            if next_end < current_time {
                return Err(Error::InvalidEndDate);
            }
            if next_end <= effective_start {
                return Err(Error::InvalidEndDate);
            }
            event.end_date = next_end;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        env.events().publish(
            (Symbol::new(&env, "event_updated"),),
            (event_id, event.organizer),
        );

        Ok(())
    }

    /// Update tickets sold count. Only callable by the ticket NFT contract.
    pub fn update_tickets_sold(env: Env, event_id: u32, amount: u128) -> Result<(), Error> {
        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        event.ticket_nft_addr.require_auth();

        event.tickets_sold = event
            .tickets_sold
            .checked_add(amount)
            .ok_or(Error::CounterOverflow)?;

        if event.tickets_sold > event.total_tickets {
            return Err(Error::CannotSellMoreTickets);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        Ok(())
    }

    /// Purchase a single ticket for a specific tier.
    /// `tier_index`: index into the event's tiers Vec. Pass 0 for single-tier events.
    /// Funds are escrowed in the contract until the organizer withdraws after the event.
    pub fn purchase_ticket(
        env: Env,
        buyer: Address,
        event_id: u32,
        tier_index: u32,
    ) -> Result<(), Error> {
        buyer.require_auth();

        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        let mut tiers: Vec<TicketTier> = env
            .storage()
            .persistent()
            .get(&DataKey::EventTiers(event_id))
            .ok_or(Error::EventNotFound)?;

        if tier_index as usize >= tiers.len() as usize {
            return Err(Error::InvalidTierIndex);
        }

        let mut tier = tiers.get(tier_index).unwrap();

        if tier.sold_quantity >= tier.total_quantity {
            return Err(Error::TierSoldOut);
        }

        let price = tier.price;

        // Escrow payment in the contract; released to organizer via withdraw_funds
        if price > 0 {
            let token_client = soroban_sdk::token::Client::new(&env, &event.payment_token);
            token_client.transfer(&buyer, &env.current_contract_address(), &price);

            let balance_key = DataKey::EventBalance(event_id);
            let current_balance: i128 = env
                .storage()
                .persistent()
                .get(&balance_key)
                .unwrap_or(0);
            env.storage()
                .persistent()
                .set(&balance_key, &(current_balance + price));
            Self::extend_persistent_ttl(&env, &balance_key);
        }

        env.invoke_contract::<u128>(
            &event.ticket_nft_addr,
            &Symbol::new(&env, "mint_ticket_nft"),
            soroban_sdk::vec![&env, buyer.clone().into_val(&env)],
        );

        tier.sold_quantity += 1;
        tiers.set(tier_index, tier);
        env.storage()
            .persistent()
            .set(&DataKey::EventTiers(event_id), &tiers);
        Self::extend_persistent_ttl(&env, &DataKey::EventTiers(event_id));

        Self::record_purchase(&env, event_id, buyer.clone(), 1, price);

        event.tickets_sold += 1;
        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        env.events().publish(
            (Symbol::new(&env, "ticket_purchased"),),
            (event_id, buyer, event.ticket_nft_addr, tier_index),
        );

        Ok(())
    }

    /// Purchase multiple tickets at once using the base ticket_price with group discounts.
    /// Funds are escrowed in the contract until the organizer withdraws after the event.
    pub fn purchase_tickets(
        env: Env,
        buyer: Address,
        event_id: u32,
        quantity: u128,
    ) -> Result<(), Error> {
        buyer.require_auth();

        if quantity == 0 {
            return Err(Error::InvalidTicketCount);
        }

        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        let next_tickets_sold = event
            .tickets_sold
            .checked_add(quantity)
            .ok_or(Error::CounterOverflow)?;

        if next_tickets_sold > event.total_tickets {
            return Err(Error::EventSoldOut);
        }

        let total_price = Self::calculate_total_price(event.ticket_price, quantity);

        // Escrow payment in the contract; released to organizer via withdraw_funds
        if total_price > 0 {
            let token_client = soroban_sdk::token::Client::new(&env, &event.payment_token);
            token_client.transfer(&buyer, &env.current_contract_address(), &total_price);

            let balance_key = DataKey::EventBalance(event_id);
            let current_balance: i128 = env
                .storage()
                .persistent()
                .get(&balance_key)
                .unwrap_or(0);
            env.storage()
                .persistent()
                .set(&balance_key, &(current_balance + total_price));
            Self::extend_persistent_ttl(&env, &balance_key);
        }

        for _ in 0..quantity {
            env.invoke_contract::<u128>(
                &event.ticket_nft_addr,
                &Symbol::new(&env, "mint_ticket_nft"),
                soroban_sdk::vec![&env, buyer.clone().into_val(&env)],
            );
        }

        Self::record_purchase(&env, event_id, buyer.clone(), quantity, total_price);

        event.tickets_sold = next_tickets_sold;
        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        env.events().publish(
            (Symbol::new(&env, "ticket_purchased"),),
            (event_id, buyer, quantity, total_price, event.ticket_nft_addr),
        );

        Ok(())
    }

    /// Withdraw accumulated ticket sale funds to the organizer wallet.
    ///
    /// Rules:
    /// - Only callable by the event organizer
    /// - Only after the event `end_date` has passed
    /// - Only if the event has not been cancelled (cancelled events use `claim_refund`)
    /// - Prevents double withdrawal via a persistent flag
    pub fn withdraw_funds(env: Env, event_id: u32) -> Result<(), Error> {
        let event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        event.organizer.require_auth();

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        if env.ledger().timestamp() <= event.end_date {
            return Err(Error::EventNotEnded);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::FundsWithdrawn(event_id))
        {
            return Err(Error::FundsAlreadyWithdrawn);
        }

        // Mark withdrawn before transfer (checks-effects-interactions pattern)
        env.storage()
            .persistent()
            .set(&DataKey::FundsWithdrawn(event_id), &true);
        Self::extend_persistent_ttl(&env, &DataKey::FundsWithdrawn(event_id));

        let balance: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::EventBalance(event_id))
            .unwrap_or(0);

        if balance > 0 {
            let token_client = soroban_sdk::token::Client::new(&env, &event.payment_token);
            token_client.transfer(&env.current_contract_address(), &event.organizer, &balance);
        }

        env.events().publish(
            (Symbol::new(&env, "funds_withdrawn"),),
            (event_id, event.organizer, balance),
        );

        Ok(())
    }

    // ========== Discount codes ==========

    /// Create a promotional discount code for an event.
    ///
    /// - Only callable by the event organizer
    /// - `percentage`: 1–100 (price reduction in percent)
    /// - `max_uses`: maximum redemptions; 0 means unlimited
    /// - `expiration`: ledger timestamp after which the code is invalid; 0 means no expiration
    pub fn create_discount(
        env: Env,
        event_id: u32,
        code: String,
        percentage: u32,
        max_uses: u32,
        expiration: u64,
    ) -> Result<(), Error> {
        let event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        event.organizer.require_auth();

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        if percentage == 0 || percentage > 100 {
            return Err(Error::InvalidDiscountPercentage);
        }

        if expiration != 0 && expiration <= env.ledger().timestamp() {
            return Err(Error::InvalidExpiration);
        }

        let key = DataKey::Discount(event_id, code.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::DiscountAlreadyExists);
        }

        let discount = DiscountCode {
            code: code.clone(),
            percentage,
            max_uses,
            uses_remaining: max_uses,
            expiration,
        };

        env.storage().persistent().set(&key, &discount);
        Self::extend_persistent_ttl(&env, &key);

        env.events().publish(
            (Symbol::new(&env, "discount_created"),),
            (event_id, code, percentage, max_uses, expiration),
        );

        Ok(())
    }

    /// Query a discount code's details.
    pub fn get_discount(env: Env, event_id: u32, code: String) -> Result<DiscountCode, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Discount(event_id, code))
            .ok_or(Error::DiscountNotFound)
    }

    /// Purchase a single ticket applying a discount code.
    ///
    /// Behaves identically to `purchase_ticket` but reduces the price by the
    /// code's percentage before charging the buyer.  The code's `uses_remaining`
    /// is decremented atomically (or rejected if it is exhausted/expired).
    pub fn purchase_ticket_with_discount(
        env: Env,
        buyer: Address,
        event_id: u32,
        tier_index: u32,
        code: String,
    ) -> Result<(), Error> {
        buyer.require_auth();

        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        let mut tiers: Vec<TicketTier> = env
            .storage()
            .persistent()
            .get(&DataKey::EventTiers(event_id))
            .ok_or(Error::EventNotFound)?;

        if tier_index as usize >= tiers.len() as usize {
            return Err(Error::InvalidTierIndex);
        }

        let mut tier = tiers.get(tier_index).unwrap();

        if tier.sold_quantity >= tier.total_quantity {
            return Err(Error::TierSoldOut);
        }

        let base_price = tier.price;

        // Validate discount and compute the final charge
        let final_price =
            Self::apply_discount_code(&env, event_id, code.clone(), base_price)?;

        if final_price > 0 {
            let token_client = soroban_sdk::token::Client::new(&env, &event.payment_token);
            token_client.transfer(&buyer, &env.current_contract_address(), &final_price);

            let balance_key = DataKey::EventBalance(event_id);
            let current_balance: i128 = env
                .storage()
                .persistent()
                .get(&balance_key)
                .unwrap_or(0);
            env.storage()
                .persistent()
                .set(&balance_key, &(current_balance + final_price));
            Self::extend_persistent_ttl(&env, &balance_key);
        }

        env.invoke_contract::<u128>(
            &event.ticket_nft_addr,
            &Symbol::new(&env, "mint_ticket_nft"),
            soroban_sdk::vec![&env, buyer.clone().into_val(&env)],
        );

        tier.sold_quantity += 1;
        tiers.set(tier_index, tier);
        env.storage()
            .persistent()
            .set(&DataKey::EventTiers(event_id), &tiers);
        Self::extend_persistent_ttl(&env, &DataKey::EventTiers(event_id));

        Self::record_purchase(&env, event_id, buyer.clone(), 1, final_price);

        event.tickets_sold += 1;
        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        env.events().publish(
            (Symbol::new(&env, "ticket_purchased"),),
            (event_id, buyer.clone(), event.ticket_nft_addr, tier_index),
        );

        env.events().publish(
            (Symbol::new(&env, "discount_applied"),),
            (event_id, buyer, code, base_price, final_price),
        );

        Ok(())
    }

    // ========== Waitlist ==========

    /// Join the waitlist for a sold-out event.
    ///
    /// Rules:
    /// - The event must be sold out (tickets_sold >= total_tickets)
    /// - The event must not be cancelled
    /// - The caller must not already be on the waitlist
    /// Emits a `waitlist_joined` event with the caller's position (1-based).
    pub fn join_waitlist(env: Env, buyer: Address, event_id: u32) -> Result<u32, Error> {
        buyer.require_auth();

        let event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        if event.is_canceled {
            return Err(Error::EventCanceled);
        }

        if event.tickets_sold < event.total_tickets {
            return Err(Error::EventNotSoldOut);
        }

        let waitlist_key = DataKey::Waitlist(event_id);
        let mut waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&waitlist_key)
            .unwrap_or_else(|| Vec::new(&env));

        // Prevent duplicate entries
        for addr in waitlist.iter() {
            if addr == buyer {
                return Err(Error::AlreadyOnWaitlist);
            }
        }

        waitlist.push_back(buyer.clone());
        let position = waitlist.len(); // 1-based position

        env.storage().persistent().set(&waitlist_key, &waitlist);
        Self::extend_persistent_ttl(&env, &waitlist_key);

        env.events().publish(
            (Symbol::new(&env, "waitlist_joined"),),
            (event_id, buyer, position),
        );

        Ok(position)
    }

    /// Return the 1-based waitlist position of `buyer` for an event, or 0 if not on the list.
    pub fn get_waitlist_position(env: Env, event_id: u32, buyer: Address) -> u32 {
        let waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Waitlist(event_id))
            .unwrap_or_else(|| Vec::new(&env));

        for (i, addr) in waitlist.iter().enumerate() {
            if addr == buyer {
                return (i as u32) + 1;
            }
        }
        0
    }

    /// Return the full waitlist for an event.
    pub fn get_waitlist(env: Env, event_id: u32) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Waitlist(event_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return a previously purchased ticket and receive a refund.
    ///
    /// Rules:
    /// - Only callable by the original ticket buyer
    /// - Event must not be cancelled (use `claim_refund` for cancelled events)
    /// - Event must not have ended
    /// - Buyer must hold at least one ticket for the given tier
    ///
    /// After the slot is freed the first person on the waitlist (if any) is
    /// automatically promoted: a `waitlist_promoted` event is emitted and the
    /// slot becomes immediately available for them to call `purchase_ticket`.
    pub fn return_ticket(
        env: Env,
        buyer: Address,
        event_id: u32,
        tier_index: u32,
    ) -> Result<(), Error> {
        buyer.require_auth();

        let mut event: Event = env
            .storage()
            .persistent()
            .get(&DataKey::Event(event_id))
            .ok_or(Error::EventNotFound)?;

        if event.is_canceled {
            return Err(Error::EventAlreadyCanceled);
        }

        // Cannot return a ticket after the event has ended
        if env.ledger().timestamp() > event.end_date {
            return Err(Error::EventNotEnded);
        }

        let purchase_key = DataKey::BuyerPurchase(event_id, buyer.clone());
        let mut purchase: BuyerPurchase = env
            .storage()
            .persistent()
            .get(&purchase_key)
            .ok_or(Error::NotABuyer)?;

        let mut tiers: Vec<TicketTier> = env
            .storage()
            .persistent()
            .get(&DataKey::EventTiers(event_id))
            .ok_or(Error::EventNotFound)?;

        if tier_index as usize >= tiers.len() as usize {
            return Err(Error::InvalidTierIndex);
        }

        let mut tier = tiers.get(tier_index).unwrap();
        if tier.sold_quantity == 0 || purchase.quantity == 0 {
            return Err(Error::NotABuyer);
        }

        let refund_amount = tier.price;

        // Update purchase record — remove entirely when quantity reaches zero
        purchase.quantity = purchase.quantity.saturating_sub(1);
        purchase.total_paid = purchase.total_paid.saturating_sub(refund_amount);
        if purchase.quantity == 0 {
            env.storage().persistent().remove(&purchase_key);
        } else {
            env.storage().persistent().set(&purchase_key, &purchase);
            Self::extend_persistent_ttl(&env, &purchase_key);
        }

        // Decrement tier sold count
        tier.sold_quantity -= 1;
        tiers.set(tier_index, tier);
        env.storage()
            .persistent()
            .set(&DataKey::EventTiers(event_id), &tiers);
        Self::extend_persistent_ttl(&env, &DataKey::EventTiers(event_id));

        // Decrement aggregate sold count
        event.tickets_sold = event.tickets_sold.saturating_sub(1);
        env.storage()
            .persistent()
            .set(&DataKey::Event(event_id), &event);
        Self::extend_persistent_ttl(&env, &DataKey::Event(event_id));

        // Refund the buyer from the escrowed balance
        if refund_amount > 0 {
            let token_client = soroban_sdk::token::Client::new(&env, &event.payment_token);
            token_client.transfer(&env.current_contract_address(), &buyer, &refund_amount);

            let balance_key = DataKey::EventBalance(event_id);
            let current_balance: i128 = env
                .storage()
                .persistent()
                .get(&balance_key)
                .unwrap_or(0);
            env.storage()
                .persistent()
                .set(&balance_key, &current_balance.saturating_sub(refund_amount));
        }

        env.events().publish(
            (Symbol::new(&env, "ticket_returned"),),
            (event_id, buyer, tier_index, refund_amount),
        );

        // Promote the next person from the waitlist now that a slot is free
        Self::try_promote_from_waitlist(&env, event_id);

        Ok(())
    }

    // ========== Private helpers ==========

    /// Validate a discount code and return the discounted price.
    /// Decrements `uses_remaining` on success.
    fn apply_discount_code(
        env: &Env,
        event_id: u32,
        code: String,
        base_price: i128,
    ) -> Result<i128, Error> {
        let key = DataKey::Discount(event_id, code);
        let mut discount: DiscountCode = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::DiscountNotFound)?;

        if discount.expiration != 0 && env.ledger().timestamp() > discount.expiration {
            return Err(Error::DiscountExpired);
        }

        if discount.max_uses > 0 && discount.uses_remaining == 0 {
            return Err(Error::DiscountMaxUsesReached);
        }

        // Decrement uses (unlimited codes skip this step)
        if discount.max_uses > 0 {
            discount.uses_remaining -= 1;
            env.storage().persistent().set(&key, &discount);
            Self::extend_persistent_ttl(env, &key);
        }

        // final_price = base_price * (100 - percentage) / 100
        let final_price = if base_price <= 0 {
            0i128
        } else {
            base_price
                .checked_mul(100i128 - discount.percentage as i128)
                .and_then(|v| v.checked_div(100))
                .unwrap_or(0)
        };

        Ok(final_price)
    }

    /// Dequeue the head of the waitlist and emit `waitlist_promoted`.
    /// No-op if the waitlist is empty.
    fn try_promote_from_waitlist(env: &Env, event_id: u32) {
        let waitlist_key = DataKey::Waitlist(event_id);
        let waitlist: Vec<Address> = env
            .storage()
            .persistent()
            .get(&waitlist_key)
            .unwrap_or_else(|| Vec::new(env));

        if waitlist.is_empty() {
            return;
        }

        let promoted = waitlist.get(0).unwrap();

        // Rebuild the list without the promoted address
        let mut remaining = Vec::new(env);
        for (i, addr) in waitlist.iter().enumerate() {
            if i > 0 {
                remaining.push_back(addr);
            }
        }
        env.storage().persistent().set(&waitlist_key, &remaining);
        Self::extend_persistent_ttl(env, &waitlist_key);

        env.events().publish(
            (Symbol::new(env, "waitlist_promoted"),),
            (event_id, promoted),
        );
    }

    fn validate_event_params(
        env: &Env,
        start_date: u64,
        end_date: u64,
        ticket_price: i128,
        total_tickets: u128,
    ) -> Result<(), Error> {
        let current_time = env.ledger().timestamp();
        if start_date <= current_time {
            return Err(Error::InvalidStartDate);
        }
        if end_date <= start_date {
            return Err(Error::InvalidEndDate);
        }
        if ticket_price < 0 {
            return Err(Error::NegativeTicketPrice);
        }
        if total_tickets == 0 {
            return Err(Error::InvalidTicketCount);
        }
        Ok(())
    }

    fn get_and_increment_counter(env: &Env) -> Result<u32, Error> {
        let current: u32 = env
            .storage()
            .instance()
            .get(&DataKey::EventCounter)
            .unwrap_or(0);
        let next = current.checked_add(1).ok_or(Error::CounterOverflow)?;
        env.storage().instance().set(&DataKey::EventCounter, &next);
        env.storage()
            .instance()
            .extend_ttl(Self::ttl_threshold(), Self::ttl_extend_to());
        Ok(current)
    }

    fn deploy_ticket_nft(env: &Env, event_id: u32) -> Option<Address> {
        let factory_addr: Address = env.storage().instance().get(&DataKey::TicketFactory)?;
        let mut salt_bytes = [0u8; 32];
        salt_bytes[28..32].copy_from_slice(&event_id.to_be_bytes());
        let salt = BytesN::from_array(env, &salt_bytes);

        Some(env.invoke_contract::<Address>(
            &factory_addr,
            &Symbol::new(env, "deploy_ticket"),
            soroban_sdk::vec![
                env,
                env.current_contract_address().into_val(env),
                salt.into_val(env)
            ],
        ))
    }

    fn record_purchase(
        env: &Env,
        event_id: u32,
        buyer: Address,
        quantity: u128,
        total_paid: i128,
    ) {
        let key = DataKey::BuyerPurchase(event_id, buyer.clone());
        let existing = env.storage().persistent().get::<_, BuyerPurchase>(&key);

        if let Some(mut purchase) = existing {
            purchase.quantity = purchase
                .quantity
                .checked_add(quantity)
                .unwrap_or_else(|| panic!("Purchase quantity overflow"));
            purchase.total_paid = purchase
                .total_paid
                .checked_add(total_paid)
                .unwrap_or_else(|| panic!("Purchase total overflow"));
            env.storage().persistent().set(&key, &purchase);
        } else {
            let purchase = BuyerPurchase {
                quantity,
                total_paid,
            };
            env.storage().persistent().set(&key, &purchase);

            let buyers_key = DataKey::EventBuyers(event_id);
            let mut buyers: Vec<Address> = env
                .storage()
                .persistent()
                .get(&buyers_key)
                .unwrap_or_else(|| Vec::new(env));
            buyers.push_back(buyer.clone());
            env.storage().persistent().set(&buyers_key, &buyers);
            Self::extend_persistent_ttl(env, &buyers_key);
        }

        Self::extend_persistent_ttl(env, &key);
    }

    fn calculate_total_price(ticket_price: i128, quantity: u128) -> i128 {
        if ticket_price <= 0 {
            return 0;
        }
        let quantity_i128 =
            i128::try_from(quantity).unwrap_or_else(|_| panic!("Quantity exceeds pricing range"));
        let subtotal = ticket_price
            .checked_mul(quantity_i128)
            .unwrap_or_else(|| panic!("Price overflow"));

        let discount_bps = if quantity >= 10 {
            1_000i128
        } else if quantity >= 5 {
            500i128
        } else {
            0i128
        };

        subtotal
            .checked_mul(10_000i128 - discount_bps)
            .and_then(|value| value.checked_div(10_000))
            .unwrap_or_else(|| panic!("Discount calculation overflow"))
    }

    fn extend_persistent_ttl(env: &Env, key: &DataKey) {
        env.storage()
            .persistent()
            .extend_ttl(key, Self::ttl_threshold(), Self::ttl_extend_to());
    }

    const fn ttl_threshold() -> u32 {
        30 * 24 * 60 * 60 / 5
    }

    const fn ttl_extend_to() -> u32 {
        100 * 24 * 60 * 60 / 5
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod update_event_tests {
    use super::*;
    use crate::test::MockContract;
    use soroban_sdk::{testutils::Address as _, testutils::Ledger};

    fn setup_event_for_update(env: &Env) -> (EventManagerClient<'_>, Address, u32) {
        let contract_id = env.register(EventManager, ());
        let client = EventManagerClient::new(env, &contract_id);
        let mock_addr = env.register(MockContract, ());
        let organizer = Address::generate(env);
        env.mock_all_auths();
        client.initialize(&mock_addr);

        let start_date = env.ledger().timestamp() + 86_400;
        let end_date = start_date + 86_400;
        let event_id = client.create_event(&CreateEventParams {
            organizer: organizer.clone(),
            theme: String::from_str(env, "Original Theme"),
            event_type: String::from_str(env, "Conference"),
            start_date,
            end_date,
            ticket_price: 1_000_0000000,
            total_tickets: 100,
            payment_token: mock_addr,
            tiers: Vec::new(env),
        });
        (client, organizer, event_id)
    }

    #[test]
    fn test_update_event_theme() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);

        client.update_event(
            &event_id,
            &Option::Some(String::from_str(&env, "Updated Theme")),
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::None,
        );

        let event = client.get_event(&event_id);
        assert_eq!(event.theme, String::from_str(&env, "Updated Theme"));
    }

    #[test]
    fn test_update_event_ticket_price() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);
        let new_price = 2_000_0000000i128;

        client.update_event(
            &event_id,
            &Option::None,
            &Option::Some(new_price),
            &Option::None,
            &Option::None,
            &Option::None,
        );

        assert_eq!(client.get_event(&event_id).ticket_price, new_price);
    }

    #[test]
    fn test_update_event_total_tickets() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);

        client.update_event(
            &event_id,
            &Option::None,
            &Option::None,
            &Option::Some(200u128),
            &Option::None,
            &Option::None,
        );

        assert_eq!(client.get_event(&event_id).total_tickets, 200);
    }

    #[test]
    fn test_update_event_dates() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);
        let new_start = env.ledger().timestamp() + 172_800;
        let new_end = new_start + 86_400;

        client.update_event(
            &event_id,
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::Some(new_start),
            &Option::Some(new_end),
        );

        let event = client.get_event(&event_id);
        assert_eq!(event.start_date, new_start);
        assert_eq!(event.end_date, new_end);
    }

    #[test]
    fn test_update_event_emits_event() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);

        client.update_event(
            &event_id,
            &Option::Some(String::from_str(&env, "Emit Test")),
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::None,
        );

        assert_eq!(
            client.get_event(&event_id).theme,
            String::from_str(&env, "Emit Test")
        );
    }

    #[test]
    fn test_update_event_canceled_fails() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);
        client.cancel_event(&event_id);

        let result = client.try_update_event(
            &event_id,
            &Option::Some(String::from_str(&env, "Should fail")),
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_update_event_total_tickets_below_sold_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(EventManager, ());
        let client = EventManagerClient::new(&env, &contract_id);
        let mock_addr = env.register(MockContract, ());
        let organizer = Address::generate(&env);
        client.initialize(&mock_addr);

        let start_date = env.ledger().timestamp() + 86_400;
        let end_date = start_date + 86_400;
        let event_id = client.create_event(&CreateEventParams {
            organizer: organizer.clone(),
            theme: String::from_str(&env, "Event"),
            event_type: String::from_str(&env, "Type"),
            start_date,
            end_date,
            ticket_price: 100i128,
            total_tickets: 10u128,
            payment_token: mock_addr,
            tiers: Vec::new(&env),
        });

        client.purchase_ticket(&Address::generate(&env), &event_id, &0u32);
        client.purchase_ticket(&Address::generate(&env), &event_id, &0u32);

        let result = client.try_update_event(
            &event_id,
            &Option::None,
            &Option::None,
            &Option::Some(1u128),
            &Option::None,
            &Option::None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_update_event_start_date_past_fails() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);
        env.ledger()
            .set_timestamp(env.ledger().timestamp() + 86_400 * 2);
        let past_start = env.ledger().timestamp() - 3_600;

        let result = client.try_update_event(
            &event_id,
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::Some(past_start),
            &Option::None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_update_event_end_before_start_fails() {
        let env = Env::default();
        let (client, _organizer, event_id) = setup_event_for_update(&env);
        let start_date = env.ledger().timestamp() + 86_400;
        let end_before_start = start_date - 3_600;

        let result = client.try_update_event(
            &event_id,
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::Some(start_date),
            &Option::Some(end_before_start),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_update_event_not_found_fails() {
        let env = Env::default();
        let contract_id = env.register(EventManager, ());
        let client = EventManagerClient::new(&env, &contract_id);
        let mock_addr = env.register(MockContract, ());
        env.mock_all_auths();
        client.initialize(&mock_addr);

        let result = client.try_update_event(
            &999u32,
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::None,
            &Option::None,
        );
        assert!(result.is_err());
    }
}
