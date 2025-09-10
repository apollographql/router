#!lua name=echolib

local function TableConcat(t1,t2)
    for i=1,#t2 do
        t1[#t1+1] = t2[i]
    end
    return t1
end

local function echo(keys, args)
  return TableConcat(keys, args)
end

redis.register_function{
  function_name='echo',
  callback=echo,
  flags={ 'no-writes' }
}